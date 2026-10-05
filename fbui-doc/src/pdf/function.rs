//! PDF functions (§7.10): sampled (0), exponential (2), stitching (3) and
//! PostScript calculator (4). Used by shadings and Separation/DeviceN tint
//! transforms.

use alloc::vec::Vec;

use super::lexer::{Lexer, Token};
use super::object::Object;
use super::Document;
// Unused when another crate in the graph links std (its inherent f32
// methods then win); needed on a pure no_std build.
#[allow(unused_imports)]
use crate::math::F32Ext;

/// Calculator program size / stack bounds.
const MAX_PROGRAM: usize = 4096;
const MAX_STACK: usize = 100;

#[derive(Debug, Clone)]
pub enum Function {
    Sampled {
        domain: Vec<f32>,
        range: Vec<f32>,
        size: Vec<u32>,
        bps: u32,
        encode: Vec<f32>,
        decode: Vec<f32>,
        samples: Vec<u8>,
    },
    Exponential {
        domain: [f32; 2],
        c0: Vec<f32>,
        c1: Vec<f32>,
        n: f32,
    },
    Stitching {
        domain: [f32; 2],
        functions: Vec<Function>,
        bounds: Vec<f32>,
        encode: Vec<f32>,
    },
    Calculator {
        domain: Vec<f32>,
        range: Vec<f32>,
        program: Vec<PsOp>,
    },
    /// Several 1-in/1-out functions, one per output (an array of functions).
    Array(Vec<Function>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PsOp {
    Num(f32),
    Op(PsKind),
    /// `{ … } if` / `{ … } { … } ifelse`: offsets of the branches' ends.
    If {
        then_len: usize,
    },
    IfElse {
        then_len: usize,
        else_len: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PsKind {
    Abs,
    Add,
    Atan,
    Ceiling,
    Cos,
    Cvi,
    Cvr,
    Div,
    Exp,
    Floor,
    Idiv,
    Ln,
    Log,
    Mod,
    Mul,
    Neg,
    Round,
    Sin,
    Sqrt,
    Sub,
    Truncate,
    And,
    Bitshift,
    Eq,
    False,
    Ge,
    Gt,
    Le,
    Lt,
    Ne,
    Not,
    Or,
    True,
    Xor,
    Copy,
    Dup,
    Exch,
    Index,
    Pop,
    Roll,
}

fn floats(doc: &Document, o: &Object) -> Vec<f32> {
    match doc.resolve(o) {
        Ok(Object::Array(a)) => a
            .iter()
            .filter_map(|x| doc.resolve(x).ok()?.as_f32())
            .collect(),
        Ok(other) => other.as_f32().into_iter().collect(),
        Err(_) => Vec::new(),
    }
}

impl Function {
    pub fn parse(doc: &Document, obj: &Object) -> Option<Function> {
        Self::parse_depth(doc, obj, 0)
    }

    fn parse_depth(doc: &Document, obj: &Object, depth: u32) -> Option<Function> {
        if depth > 8 {
            return None;
        }
        let obj = doc.resolve(obj).ok()?;
        if let Object::Array(a) = &obj {
            let fs: Option<Vec<_>> = a
                .iter()
                .map(|f| Self::parse_depth(doc, f, depth + 1))
                .collect();
            return Some(Function::Array(fs?));
        }
        let d = obj.as_dict()?;
        let ty = doc.get_in(d, b"FunctionType").as_i64()?;
        let domain = floats(doc, d.get(b"Domain").unwrap_or(&Object::Null));
        let dom2 = [
            domain.first().copied().unwrap_or(0.0),
            domain.get(1).copied().unwrap_or(1.0),
        ];
        match ty {
            0 => {
                let s = obj.as_stream()?;
                let size: Vec<u32> = floats(doc, d.get(b"Size")?)
                    .iter()
                    .map(|&v| (v as u32).clamp(1, 1 << 16))
                    .collect();
                let range = floats(doc, d.get(b"Range")?);
                let bps = doc.get_in(d, b"BitsPerSample").as_i64()? as u32;
                if !matches!(bps, 1 | 2 | 4 | 8 | 12 | 16 | 24 | 32) || size.is_empty() {
                    return None;
                }
                let mut encode = floats(doc, d.get(b"Encode").unwrap_or(&Object::Null));
                if encode.len() < size.len() * 2 {
                    encode = size.iter().flat_map(|&s| [0.0, (s - 1) as f32]).collect();
                }
                let mut decode = floats(doc, d.get(b"Decode").unwrap_or(&Object::Null));
                if decode.len() < range.len() {
                    decode = range.clone();
                }
                let samples = doc.stream_bytes(s).ok()?;
                Some(Function::Sampled {
                    domain,
                    range,
                    size,
                    bps,
                    encode,
                    decode,
                    samples,
                })
            }
            2 => {
                let c0 = d
                    .get(b"C0")
                    .map(|o| floats(doc, o))
                    .unwrap_or_else(|| alloc::vec![0.0]);
                let c1 = d
                    .get(b"C1")
                    .map(|o| floats(doc, o))
                    .unwrap_or_else(|| alloc::vec![1.0]);
                let n = doc.get_in(d, b"N").as_f32().unwrap_or(1.0);
                Some(Function::Exponential {
                    domain: dom2,
                    c0,
                    c1,
                    n,
                })
            }
            3 => {
                let fs = doc.get_in(d, b"Functions");
                let functions: Option<Vec<_>> = fs
                    .as_array()?
                    .iter()
                    .map(|f| Self::parse_depth(doc, f, depth + 1))
                    .collect();
                let functions = functions?;
                let bounds = floats(doc, d.get(b"Bounds").unwrap_or(&Object::Null));
                let encode = floats(doc, d.get(b"Encode").unwrap_or(&Object::Null));
                if functions.is_empty() || bounds.len() + 1 != functions.len() {
                    return None;
                }
                Some(Function::Stitching {
                    domain: dom2,
                    functions,
                    bounds,
                    encode,
                })
            }
            4 => {
                let s = obj.as_stream()?;
                let code = doc.stream_bytes(s).ok()?;
                let range = floats(doc, d.get(b"Range")?);
                let mut lx = Lexer::new(&code);
                if lx.next_token() != Some(Token::BraceOpen) {
                    return None;
                }
                let program = parse_ps(&mut lx, 0)?;
                Some(Function::Calculator {
                    domain,
                    range,
                    program,
                })
            }
            _ => None,
        }
    }

    /// Evaluate at `input`; outputs are written to the front of the result.
    pub fn eval(&self, input: &[f32]) -> Vec<f32> {
        match self {
            Function::Array(fs) => fs
                .iter()
                .flat_map(|f| f.eval(input).into_iter().take(1))
                .collect(),
            Function::Exponential { domain, c0, c1, n } => {
                let x = input
                    .first()
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(domain[0], domain[1]);
                let xn = if *n == 1.0 { x } else { x.max(0.0).powf(*n) };
                c0.iter()
                    .zip(c1.iter())
                    .map(|(a, b)| a + xn * (b - a))
                    .collect()
            }
            Function::Stitching {
                domain,
                functions,
                bounds,
                encode,
            } => {
                let x = input
                    .first()
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(domain[0], domain[1]);
                let k = bounds.iter().position(|&b| x < b).unwrap_or(bounds.len());
                let lo = if k == 0 { domain[0] } else { bounds[k - 1] };
                let hi = if k == bounds.len() {
                    domain[1]
                } else {
                    bounds[k]
                };
                let (e0, e1) = (
                    encode.get(2 * k).copied().unwrap_or(0.0),
                    encode.get(2 * k + 1).copied().unwrap_or(1.0),
                );
                let t = if hi > lo {
                    e0 + (x - lo) * (e1 - e0) / (hi - lo)
                } else {
                    e0
                };
                functions[k].eval(&[t])
            }
            Function::Sampled {
                domain,
                range,
                size,
                bps,
                encode,
                decode,
                samples,
            } => {
                let m = size.len();
                let n = range.len() / 2;
                // Multilinear in the first input only; nearest for the rest
                // (shadings and tint transforms are almost always 1-in).
                let mut idx = Vec::with_capacity(m);
                let mut frac = 0.0;
                for i in 0..m {
                    let x = input.get(i).copied().unwrap_or(0.0);
                    let (d0, d1) = (
                        domain.get(2 * i).copied().unwrap_or(0.0),
                        domain.get(2 * i + 1).copied().unwrap_or(1.0),
                    );
                    let x = x.clamp(d0.min(d1), d0.max(d1));
                    let e = encode[2 * i]
                        + if d1 != d0 {
                            (x - d0) * (encode[2 * i + 1] - encode[2 * i]) / (d1 - d0)
                        } else {
                            0.0
                        };
                    let e = e.clamp(0.0, (size[i] - 1) as f32);
                    if i == 0 {
                        frac = e - e.floor();
                    }
                    idx.push(e.floor() as u32);
                }
                let sample_at = |idx: &[u32], j: usize| -> f32 {
                    let mut pos = 0usize;
                    let mut stride = 1usize;
                    for (i, &v) in idx.iter().enumerate() {
                        pos += v as usize * stride;
                        stride *= size[i] as usize;
                    }
                    let bit = (pos * n + j) * *bps as usize;
                    let raw = read_bits(samples, bit, *bps);
                    let max = if *bps >= 32 {
                        u32::MAX as f32
                    } else {
                        ((1u64 << bps) - 1) as f32
                    };
                    let (dl, dh) = (decode[2 * j], decode[2 * j + 1]);
                    dl + raw as f32 * (dh - dl) / max
                };
                (0..n)
                    .map(|j| {
                        let a = sample_at(&idx, j);
                        let v = if frac > 0.0 && idx[0] + 1 < size[0] {
                            let mut idx2 = idx.clone();
                            idx2[0] += 1;
                            a + (sample_at(&idx2, j) - a) * frac
                        } else {
                            a
                        };
                        v.clamp(
                            range[2 * j].min(range[2 * j + 1]),
                            range[2 * j].max(range[2 * j + 1]),
                        )
                    })
                    .collect()
            }
            Function::Calculator {
                domain,
                range,
                program,
            } => {
                let mut stack: Vec<f32> = input
                    .iter()
                    .enumerate()
                    .map(|(i, &x)| {
                        let (a, b) = (
                            domain.get(2 * i).copied().unwrap_or(0.0),
                            domain.get(2 * i + 1).copied().unwrap_or(1.0),
                        );
                        x.clamp(a.min(b), a.max(b))
                    })
                    .collect();
                run_ps(program, &mut stack);
                let n = range.len() / 2;
                let start = stack.len().saturating_sub(n);
                let mut out: Vec<f32> = stack[start..].to_vec();
                out.resize(n, 0.0);
                for (j, v) in out.iter_mut().enumerate() {
                    *v = v.clamp(
                        range[2 * j].min(range[2 * j + 1]),
                        range[2 * j].max(range[2 * j + 1]),
                    );
                }
                out
            }
        }
    }
}

fn read_bits(data: &[u8], bit: usize, n: u32) -> u32 {
    let mut v = 0u32;
    for i in 0..n as usize {
        let b = bit + i;
        let byte = data.get(b / 8).copied().unwrap_or(0);
        v = v << 1 | ((byte >> (7 - b % 8)) & 1) as u32;
    }
    v
}

fn parse_ps(lx: &mut Lexer, depth: u32) -> Option<Vec<PsOp>> {
    if depth > 16 {
        return None;
    }
    let mut out = Vec::new();
    loop {
        if out.len() > MAX_PROGRAM {
            return None;
        }
        match lx.next_token()? {
            Token::BraceClose => return Some(out),
            Token::Int(i) => out.push(PsOp::Num(i as f32)),
            Token::Real(r) => out.push(PsOp::Num(r)),
            Token::BraceOpen => {
                let then = parse_ps(lx, depth + 1)?;
                // Either `if`, or a second block then `ifelse`.
                match lx.next_token()? {
                    Token::Keyword(b"if") => {
                        out.push(PsOp::If {
                            then_len: then.len(),
                        });
                        out.extend(then);
                    }
                    Token::BraceOpen => {
                        let els = parse_ps(lx, depth + 1)?;
                        if lx.next_token()? != Token::Keyword(b"ifelse") {
                            return None;
                        }
                        out.push(PsOp::IfElse {
                            then_len: then.len(),
                            else_len: els.len(),
                        });
                        out.extend(then);
                        out.extend(els);
                    }
                    _ => return None,
                }
            }
            Token::Keyword(k) => {
                use PsKind::*;
                let op = match k {
                    b"abs" => Abs,
                    b"add" => Add,
                    b"atan" => Atan,
                    b"ceiling" => Ceiling,
                    b"cos" => Cos,
                    b"cvi" => Cvi,
                    b"cvr" => Cvr,
                    b"div" => Div,
                    b"exp" => Exp,
                    b"floor" => Floor,
                    b"idiv" => Idiv,
                    b"ln" => Ln,
                    b"log" => Log,
                    b"mod" => Mod,
                    b"mul" => Mul,
                    b"neg" => Neg,
                    b"round" => Round,
                    b"sin" => Sin,
                    b"sqrt" => Sqrt,
                    b"sub" => Sub,
                    b"truncate" => Truncate,
                    b"and" => And,
                    b"bitshift" => Bitshift,
                    b"eq" => Eq,
                    b"false" => False,
                    b"ge" => Ge,
                    b"gt" => Gt,
                    b"le" => Le,
                    b"lt" => Lt,
                    b"ne" => Ne,
                    b"not" => Not,
                    b"or" => Or,
                    b"true" => True,
                    b"xor" => Xor,
                    b"copy" => Copy,
                    b"dup" => Dup,
                    b"exch" => Exch,
                    b"index" => Index,
                    b"pop" => Pop,
                    b"roll" => Roll,
                    _ => return None,
                };
                out.push(PsOp::Op(op));
            }
            _ => return None,
        }
    }
}

fn run_ps(program: &[PsOp], st: &mut Vec<f32>) {
    let mut pc = 0;
    let mut steps = 0;
    while pc < program.len() {
        steps += 1;
        if steps > 100_000 || st.len() > MAX_STACK {
            return;
        }
        match &program[pc] {
            PsOp::Num(v) => st.push(*v),
            PsOp::If { then_len } => {
                let c = st.pop().unwrap_or(0.0);
                if c == 0.0 {
                    pc += then_len;
                }
            }
            PsOp::IfElse { then_len, else_len } => {
                let c = st.pop().unwrap_or(0.0);
                if c != 0.0 {
                    // Run `then`, then jump over `else`.
                    let then = &program[pc + 1..pc + 1 + then_len];
                    run_ps(then, st);
                } else {
                    let els = &program[pc + 1 + then_len..pc + 1 + then_len + else_len];
                    run_ps(els, st);
                }
                pc += then_len + else_len;
            }
            PsOp::Op(op) => ps_op(*op, st),
        }
        pc += 1;
    }
}

fn ps_op(op: PsKind, st: &mut Vec<f32>) {
    use PsKind::*;
    let pop = |st: &mut Vec<f32>| st.pop().unwrap_or(0.0);
    let b = |v: bool| if v { 1.0 } else { 0.0 };
    match op {
        Abs => {
            let a = pop(st);
            st.push(a.abs())
        }
        Neg => {
            let a = pop(st);
            st.push(-a)
        }
        Ceiling => {
            let a = pop(st);
            st.push(a.ceil())
        }
        Floor => {
            let a = pop(st);
            st.push(a.floor())
        }
        Round => {
            let a = pop(st);
            st.push((a + 0.5).floor())
        }
        Truncate | Cvi => {
            let a = pop(st);
            st.push(a.trunc())
        }
        Cvr => {}
        Sqrt => {
            let a = pop(st);
            st.push(a.max(0.0).sqrt())
        }
        Sin => {
            let a = pop(st);
            st.push(a.to_radians().sin())
        }
        Cos => {
            let a = pop(st);
            st.push(a.to_radians().cos())
        }
        Ln => {
            let a = pop(st);
            st.push(a.ln())
        }
        Log => {
            let a = pop(st);
            st.push(a.log10())
        }
        Not => {
            let a = pop(st);
            st.push(if a == 0.0 {
                1.0
            } else if a == 1.0 {
                0.0
            } else {
                !(a as i32) as f32
            })
        }
        True => st.push(1.0),
        False => st.push(0.0),
        Dup => {
            let a = st.last().copied().unwrap_or(0.0);
            st.push(a)
        }
        Pop => {
            st.pop();
        }
        Exch => {
            let a = pop(st);
            let c = pop(st);
            st.push(a);
            st.push(c)
        }
        Index => {
            let n = pop(st) as usize;
            let v = st.len().checked_sub(n + 1).map(|i| st[i]).unwrap_or(0.0);
            st.push(v)
        }
        Copy => {
            let n = (pop(st) as usize).min(st.len());
            let start = st.len() - n;
            for i in start..start + n {
                st.push(st[i]);
            }
        }
        Roll => {
            let j = pop(st) as i64;
            let n = (pop(st) as usize).min(st.len());
            if n > 0 {
                let start = st.len() - n;
                let k = j.rem_euclid(n as i64) as usize;
                st[start..].rotate_right(k);
            }
        }
        _ => {
            let y = pop(st);
            let x = pop(st);
            st.push(match op {
                Add => x + y,
                Sub => x - y,
                Mul => x * y,
                Div => {
                    if y != 0.0 {
                        x / y
                    } else {
                        0.0
                    }
                }
                Idiv => {
                    if y as i64 != 0 {
                        ((x as i64) / (y as i64)) as f32
                    } else {
                        0.0
                    }
                }
                Mod => {
                    if y as i64 != 0 {
                        ((x as i64) % (y as i64)) as f32
                    } else {
                        0.0
                    }
                }
                Exp => x.powf(y),
                Atan => {
                    let d = y.atan2(x).to_degrees();
                    if d < 0.0 {
                        d + 360.0
                    } else {
                        d
                    }
                }
                Eq => b(x == y),
                Ne => b(x != y),
                Ge => b(x >= y),
                Gt => b(x > y),
                Le => b(x <= y),
                Lt => b(x < y),
                And => ((x as i64) & (y as i64)) as f32,
                Or => ((x as i64) | (y as i64)) as f32,
                Xor => ((x as i64) ^ (y as i64)) as f32,
                Bitshift => {
                    let (v, s) = (x as i64, y as i64);
                    (if s >= 0 {
                        v << s.min(31)
                    } else {
                        v >> (-s).min(31)
                    }) as f32
                }
                _ => 0.0,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculator_runs_ifelse() {
        let mut lx = Lexer::new(b"{ dup 0.5 gt { 1 sub } { 2 mul } ifelse }");
        lx.next_token();
        let prog = parse_ps(&mut lx, 0).unwrap();
        let mut st = alloc::vec![0.75];
        run_ps(&prog, &mut st);
        assert_eq!(st, [0.75 - 1.0]);
        let mut st = alloc::vec![0.25];
        run_ps(&prog, &mut st);
        assert_eq!(st, [0.5]);
    }

    #[test]
    fn exponential_interpolates() {
        let f = Function::Exponential {
            domain: [0.0, 1.0],
            c0: alloc::vec![0.0, 1.0],
            c1: alloc::vec![1.0, 0.0],
            n: 1.0,
        };
        assert_eq!(f.eval(&[0.25]), [0.25, 0.75]);
    }
}
