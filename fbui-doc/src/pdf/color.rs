//! Colour spaces (§8.6), converted to sRGB-ish device RGB the simple way:
//! no ICC transforms, ICC profiles fall back to their component count.

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::function::Function;
use super::object::{Dict, Object};
use super::Document;

#[derive(Debug, Clone)]
pub enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
    /// CIE L*a*b* (with the default D50-ish white point).
    Lab {
        range: [f32; 4],
    },
    Indexed {
        base: Box<ColorSpace>,
        hival: u32,
        lookup: Vec<u8>,
    },
    /// Separation / DeviceN: tint components through a function into `alt`.
    Tint {
        n: usize,
        alt: Box<ColorSpace>,
        func: Option<Function>,
    },
    /// Pattern colour space: colour comes from a pattern resource.
    Pattern,
}

impl ColorSpace {
    /// Parse a colour space object; names are looked up in `resources`'
    /// `/ColorSpace` dict first.
    pub fn parse(doc: &Document, obj: &Object, resources: Option<&Dict>) -> ColorSpace {
        Self::parse_depth(doc, obj, resources, 0).unwrap_or(ColorSpace::Gray)
    }

    fn parse_depth(
        doc: &Document,
        obj: &Object,
        res: Option<&Dict>,
        depth: u32,
    ) -> Option<ColorSpace> {
        if depth > 8 {
            return None;
        }
        let obj = doc.resolve(obj).ok()?;
        match &obj {
            Object::Name(n) => Some(match n.as_slice() {
                b"DeviceGray" | b"G" | b"CalGray" => ColorSpace::Gray,
                b"DeviceRGB" | b"RGB" | b"CalRGB" => ColorSpace::Rgb,
                b"DeviceCMYK" | b"CMYK" => ColorSpace::Cmyk,
                b"Pattern" => ColorSpace::Pattern,
                other => {
                    let named = res
                        .map(|r| doc.get_in(r, b"ColorSpace"))
                        .and_then(|cs| cs.as_dict().map(|d| doc.get_in(d, other)))?;
                    return Self::parse_depth(doc, &named, res, depth + 1);
                }
            }),
            Object::Array(a) => {
                let fam = a.first().and_then(Object::as_name)?;
                Some(match fam {
                    b"DeviceGray" | b"CalGray" => ColorSpace::Gray,
                    b"DeviceRGB" | b"CalRGB" => ColorSpace::Rgb,
                    b"DeviceCMYK" => ColorSpace::Cmyk,
                    b"Lab" => {
                        let d = a
                            .get(1)
                            .map(|o| doc.resolve(o).unwrap_or_default())
                            .unwrap_or_default();
                        let mut range = [-100.0, 100.0, -100.0, 100.0];
                        if let Some(r) = d.as_dict().and_then(|d| {
                            doc.get_in(d, b"Range").as_array().map(<[Object]>::to_vec)
                        }) {
                            for (i, v) in r.iter().take(4).enumerate() {
                                range[i] = v.as_f32().unwrap_or(range[i]);
                            }
                        }
                        ColorSpace::Lab { range }
                    }
                    b"ICCBased" => {
                        let s = doc.resolve(a.get(1)?).ok()?;
                        let d = s.as_dict()?;
                        if let Some(alt) = d.get(b"Alternate") {
                            if let Some(cs) = Self::parse_depth(doc, alt, res, depth + 1) {
                                return Some(cs);
                            }
                        }
                        match doc.get_in(d, b"N").as_i64() {
                            Some(1) => ColorSpace::Gray,
                            Some(4) => ColorSpace::Cmyk,
                            _ => ColorSpace::Rgb,
                        }
                    }
                    b"Indexed" | b"I" => {
                        let base = Self::parse_depth(doc, a.get(1)?, res, depth + 1)?;
                        let hival = doc.resolve(a.get(2)?).ok()?.as_i64()?.clamp(0, 255) as u32;
                        let lookup = match doc.resolve(a.get(3)?).ok()? {
                            Object::String(s) => s,
                            Object::Stream(s) => doc.stream_bytes(&s).ok()?,
                            _ => return None,
                        };
                        ColorSpace::Indexed {
                            base: Box::new(base),
                            hival,
                            lookup,
                        }
                    }
                    b"Separation" | b"DeviceN" => {
                        let n = if fam == b"Separation" {
                            1
                        } else {
                            doc.resolve(a.get(1)?)
                                .ok()?
                                .as_array()
                                .map_or(1, <[Object]>::len)
                        };
                        let alt = Self::parse_depth(doc, a.get(2)?, res, depth + 1)
                            .unwrap_or(ColorSpace::Gray);
                        let func = a.get(3).and_then(|f| Function::parse(doc, f));
                        ColorSpace::Tint {
                            n,
                            alt: Box::new(alt),
                            func,
                        }
                    }
                    b"Pattern" => ColorSpace::Pattern,
                    _ => return None,
                })
            }
            _ => None,
        }
    }

    pub fn components(&self) -> usize {
        match self {
            ColorSpace::Gray | ColorSpace::Indexed { .. } | ColorSpace::Pattern => 1,
            ColorSpace::Rgb | ColorSpace::Lab { .. } => 3,
            ColorSpace::Cmyk => 4,
            ColorSpace::Tint { n, .. } => *n,
        }
    }

    /// The initial colour when this space is selected (§8.6.8).
    pub fn initial(&self) -> Vec<f32> {
        match self {
            ColorSpace::Cmyk => alloc::vec![0.0, 0.0, 0.0, 1.0],
            ColorSpace::Tint { n, .. } => alloc::vec![1.0; *n],
            ColorSpace::Lab { .. } => alloc::vec![0.0, 0.0, 0.0],
            other => alloc::vec![0.0; other.components()],
        }
    }

    /// Default `Decode` range for component `i` at `bpc` bits.
    pub fn decode_range(&self, i: usize, bpc: u32) -> (f32, f32) {
        match self {
            ColorSpace::Indexed { .. } => (0.0, ((1u32 << bpc.min(16)) - 1) as f32),
            ColorSpace::Lab { range } => match i {
                0 => (0.0, 100.0),
                1 => (range[0], range[1]),
                _ => (range[2], range[3]),
            },
            _ => (0.0, 1.0),
        }
    }

    /// Convert to RGB in 0..=1.
    pub fn to_rgb(&self, c: &[f32]) -> [f32; 3] {
        let g = |i: usize| c.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        match self {
            ColorSpace::Gray | ColorSpace::Pattern => [g(0), g(0), g(0)],
            ColorSpace::Rgb => [g(0), g(1), g(2)],
            ColorSpace::Cmyk => {
                let k = g(3);
                [
                    (1.0 - g(0)) * (1.0 - k),
                    (1.0 - g(1)) * (1.0 - k),
                    (1.0 - g(2)) * (1.0 - k),
                ]
            }
            ColorSpace::Lab { .. } => lab_to_rgb(
                c.first().copied().unwrap_or(0.0),
                c.get(1).copied().unwrap_or(0.0),
                c.get(2).copied().unwrap_or(0.0),
            ),
            ColorSpace::Indexed {
                base,
                hival,
                lookup,
            } => {
                let idx = (c.first().copied().unwrap_or(0.0).max(0.0) as u32).min(*hival) as usize;
                let n = base.components();
                let mut comps = [0.0f32; 4];
                for (j, slot) in comps.iter_mut().enumerate().take(n.min(4)) {
                    let v = lookup.get(idx * n + j).copied().unwrap_or(0) as f32 / 255.0;
                    let (lo, hi) = base.decode_range(j, 8);
                    *slot = if matches!(**base, ColorSpace::Lab { .. }) {
                        lo + v * (hi - lo)
                    } else {
                        v
                    };
                }
                base.to_rgb(&comps[..n.min(4)])
            }
            ColorSpace::Tint { alt, func, .. } => match func {
                Some(f) => alt.to_rgb(&f.eval(c)),
                // No usable tint transform: show the tint as gray ink.
                None => {
                    let t = g(0);
                    [1.0 - t, 1.0 - t, 1.0 - t]
                }
            },
        }
    }
}

fn lab_to_rgb(l: f32, a: f32, b: f32) -> [f32; 3] {
    // Unused when another crate in the graph links std (its inherent f32
    // methods then win); needed on a pure no_std build.
    #[allow(unused_imports)]
    use crate::math::F32Ext;
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let finv = |t: f32| {
        if t > 6.0 / 29.0 {
            t * t * t
        } else {
            3.0 * (6.0f32 / 29.0).powi(2) * (t - 4.0 / 29.0)
        }
    };
    // D50 white, then Bradford-adapted XYZ(D50) → linear sRGB.
    let (x, y, z) = (0.9642 * finv(fx), finv(fy), 0.8249 * finv(fz));
    let r = 3.1339 * x - 1.6169 * y - 0.4906 * z;
    let g = -0.9788 * x + 1.9161 * y + 0.0335 * z;
    let bl = 0.0719 * x - 0.2290 * y + 1.4052 * z;
    let gamma = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    [gamma(r), gamma(g), gamma(bl)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_conversions() {
        assert_eq!(
            ColorSpace::Cmyk.to_rgb(&[0.0, 0.0, 0.0, 1.0]),
            [0.0, 0.0, 0.0]
        );
        assert_eq!(
            ColorSpace::Cmyk.to_rgb(&[1.0, 0.0, 0.0, 0.0]),
            [0.0, 1.0, 1.0]
        );
        let idx = ColorSpace::Indexed {
            base: Box::new(ColorSpace::Rgb),
            hival: 1,
            lookup: alloc::vec![255, 0, 0, 0, 0, 255],
        };
        assert_eq!(idx.to_rgb(&[1.0]), [0.0, 0.0, 1.0]);
        let white = lab_to_rgb(100.0, 0.0, 0.0);
        assert!(white.iter().all(|&v| v > 0.98), "{white:?}");
    }
}
