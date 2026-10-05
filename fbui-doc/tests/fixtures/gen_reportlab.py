import sys
from reportlab.pdfgen import canvas
from reportlab.lib.colors import Color, red, blue, green, black, white, HexColor
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
out = sys.argv[1]
c = canvas.Canvas(out, pagesize=(400, 300), pageCompression=1)
c.setTitle("fbui-doc fixture")
# --- page 1: vector graphics
c.setFillColor(HexColor("#1e3a5f")); c.rect(0, 250, 400, 50, fill=1, stroke=0)
c.setFillColor(white); c.setFont("Helvetica-Bold", 20); c.drawString(15, 268, "Shapes & paint")
c.setFillColor(red); c.rect(20, 160, 70, 60, fill=1, stroke=0)
c.setStrokeColor(blue); c.setLineWidth(4); c.setDash(8, 4); c.line(110, 160, 190, 220); c.setDash()
c.setFillColor(green); c.setStrokeColor(black); c.setLineWidth(2); c.circle(240, 190, 30, fill=1, stroke=1)
c.roundRect(290, 160, 90, 60, 12, fill=0, stroke=1)
c.saveState()
p = c.beginPath(); p.rect(20, 40, 160, 90); c.clipPath(p, stroke=0, fill=0)
c.linearGradient(20, 40, 180, 40, (HexColor("#ff8800"), HexColor("#2200ff")))
c.restoreState()
c.saveState()
p = c.beginPath(); p.circle(270, 85, 45); c.clipPath(p, stroke=0, fill=0)
c.radialGradient(270, 85, 45, (white, HexColor("#008844")))
c.restoreState()
c.setFillColor(Color(0, 0, 1, alpha=0.4)); c.rect(150, 60, 100, 50, fill=1, stroke=0)
c.saveState(); c.translate(340, 60); c.rotate(30); c.setFillColor(black); c.setFont("Times-Italic", 14); c.drawString(0, 0, "rotated"); c.restoreState()
c.showPage()
# --- page 2: fonts
pdfmetrics.registerFont(TTFont("DejaVu", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"))
afm = "/usr/share/fonts/X11/Type1/c0648bt_.afm"; pfb = "/usr/share/fonts/X11/Type1/c0648bt_.pfb"
face = pdfmetrics.EmbeddedType1Face(afm, pfb)
pdfmetrics.registerTypeFace(face)
pdfmetrics.registerFont(pdfmetrics.Font("CharterEmb", face.name, "WinAnsiEncoding"))
y = 270
for font, text in [("Helvetica", "Helvetica (standard 14, not embedded)"),
                   ("Times-Roman", "Times-Roman: The quick brown fox"),
                   ("Courier", "Courier: monospaced 0123456789"),
                   ("DejaVu", "DejaVu TrueType embedded: åéîõü ÆØ €"),
                   ("CharterEmb", "Charter Type 1 embedded: fiﬂ accents é ü")]:
    c.setFont(font, 14); c.setFillColor(black); c.drawString(15, y, text); y -= 30
c.setFont("Helvetica", 11)
t = c.beginText(15, y); t.setCharSpace(1); t.textLine("Char spacing 1"); t.setCharSpace(0); t.setWordSpace(6); t.textLine("Word spacing six here"); c.drawText(t)
t = c.beginText(15, 40); t.setTextRenderMode(1); t.setFont("Helvetica-Bold", 28); c.setStrokeColor(red); t.textOut("Outlined"); c.drawText(t)
c.showPage()
c.save()
