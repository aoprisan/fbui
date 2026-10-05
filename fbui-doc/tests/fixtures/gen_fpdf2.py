import sys
from fpdf import FPDF
from PIL import Image, ImageDraw
img = Image.new("RGB", (160, 100))
d = ImageDraw.Draw(img)
for x in range(160):
    d.line([(x, 0), (x, 99)], fill=(int(255 * x / 160), 90, 255 - int(255 * x / 160)))
d.ellipse([50, 20, 110, 80], fill=(255, 220, 0))
img.save(sys.argv[2] + "/swatch.png"); img.save(sys.argv[2] + "/swatch.jpg", quality=85)
rgba = Image.new("RGBA", (60, 60), (0, 0, 0, 0)); ImageDraw.Draw(rgba).ellipse([5, 5, 55, 55], fill=(200, 0, 60, 160))
rgba.save(sys.argv[2] + "/alpha.png")

pdf = FPDF(unit="pt", format=(420, 595))
pdf.set_title("fbui-doc fpdf2 fixture")
pdf.add_font("DejaVu", "", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf")
pdf.add_font("DejaVu", "B", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf")
pdf.add_font("Serif", "", "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf")
pdf.add_page()
pdf.set_font("DejaVu", "B", 20); pdf.set_text_color(30, 58, 95)
pdf.cell(0, 30, "Bare-metal document viewer", new_x="LMARGIN", new_y="NEXT")
pdf.set_font("Serif", "", 11); pdf.set_text_color(0, 0, 0)
pdf.multi_cell(0, 15, "This page exercises CID-keyed TrueType fonts (Type 0, Identity-H) "
               "with ToUnicode maps, a table, a PNG, a JPEG and a PNG with an alpha "
               "channel (an SMask). Accents: café, naïve, Zürich, Ærøskøbing — €, ½, ≤ ≥.")
pdf.ln(6)
pdf.set_font("DejaVu", "", 10)
with pdf.table(col_widths=(80, 220)) as t:
    for row in [("Format", "Status"), ("PNG", "zune-png, every colour type"), ("JPEG", "zune-jpeg"), ("PDF", "subset renderer")]:
        r = t.row()
        for cell in row: r.cell(cell)
pdf.ln(8)
pdf.image(sys.argv[2] + "/swatch.png", x=30, w=160)
pdf.image(sys.argv[2] + "/swatch.jpg", x=210, y=pdf.get_y() - 100, w=160)
pdf.image(sys.argv[2] + "/alpha.png", x=150, y=pdf.get_y() - 80, w=60)
pdf.ln(10)
pdf.set_draw_color(0, 120, 200); pdf.set_line_width(2); pdf.line(30, pdf.get_y(), 390, pdf.get_y())
pdf.add_page(); pdf.set_font("DejaVu", "", 14)
pdf.cell(0, 20, "Second page", new_x="LMARGIN", new_y="NEXT")
pdf.set_font("Serif", "", 10)
pdf.multi_cell(0, 13, ("Lorem ipsum dolor sit amet, consectetur adipiscing elit. " * 30))
pdf.output(sys.argv[1])
