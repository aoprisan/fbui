# fbui-doc PDF fixtures

Real producer output, so the renderer is tested against what tools actually
write rather than hand-made PDFs. Regenerate with Python 3 and
`pip install reportlab fpdf2 pikepdf`:

```sh
cd fbui-doc/tests/fixtures
python3 gen_reportlab.py reportlab.pdf          # needs /usr/share/fonts DejaVu + X11 Type1 (Charter)
python3 gen_fpdf2.py fpdf2.pdf "$(mktemp -d)"   # needs DejaVu
python3 - <<'PY'
import pikepdf
with pikepdf.open("fpdf2.pdf") as p:
    p.save("objstm.pdf", object_stream_mode=pikepdf.ObjectStreamMode.generate)
b = bytearray(open("reportlab.pdf", "rb").read())
i = b.rfind(b"startxref"); j = b.find(b"\n", i + 10)
b[i + 10:j] = b"999999"
open("broken-xref.pdf", "wb").write(bytes(b))
PY
```

| file | exercises |
|---|---|
| `reportlab.pdf` | page 1: fills, dashed strokes, curves, clipping, axial + radial shadings, `ExtGState` alpha, rotated text. Page 2: standard-14 fonts (not embedded → fallback face), embedded TrueType (simple), embedded **Type 1** (Bitstream Charter, with `seac` accents), char/word spacing, text render mode 1 |
| `fpdf2.pdf` | Type 0 / CIDFontType2 / Identity-H fonts with ToUnicode, a table, PNG (Flate + predictor), JPEG (DCT), a PNG with alpha (`SMask`), a second text-heavy page |
| `objstm.pdf` | `fpdf2.pdf` rewritten by pikepdf with a cross-reference **stream** and **object streams** (PDF 1.5) |
| `broken-xref.pdf` | `reportlab.pdf` with a corrupt `startxref`, forcing the reconstruction scan |

Compare against poppler while working on the renderer:

```sh
cargo run -p fbui-doc --features std --example pdf2png -- reportlab.pdf ours.png 2 1.5
pdftoppm -r 108 -f 2 -l 2 -png -singlefile reportlab.pdf ref
```
