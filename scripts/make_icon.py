"""Ikona aplikace z předlohy assets/icon.jpg (zlaté kostky s tučňákem na tmavém pozadí).

Ořízne logo na čtverec, dá mu tmavé zaoblené pozadí (čitelné na světlém i tmavém hlavním
panelu) a vytvoří:
  assets/icon.png           512 px – README
  assets/icon/<n>.rgba      surová RGBA data pro EXE, okno, oznamovací oblast a oznámení
                            (načítá je src/icon_art.rs přes include_bytes)

Spuštění (Python 3 + Pillow):  python scripts/make_icon.py
"""
from pathlib import Path
from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "assets" / "icon.jpg"
OUT = ROOT / "assets"
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
BIG = 1024
BG = (27, 25, 22, 255)


def logo_box(img: Image.Image) -> tuple[int, int, int, int]:
    """Obdélník kolem zlatých pixelů (jas nad prahem)."""
    gray = img.convert("L").point(lambda v: 255 if v > 90 else 0)
    box = gray.getbbox()
    if not box:
        raise SystemExit("logo v předloze nenalezeno")
    return box


def main() -> None:
    img = Image.open(SRC).convert("RGB")
    x0, y0, x1, y1 = logo_box(img)
    # čtverec kolem loga s okrajem
    side = int(max(x1 - x0, y1 - y0) * 1.34)
    cx, cy = (x0 + x1) // 2, (y0 + y1) // 2
    crop = Image.new("RGB", (side, side), BG[:3])
    crop.paste(img.crop((cx - side // 2, cy - side // 2, cx - side // 2 + side, cy - side // 2 + side)), (0, 0))
    # pozadí předlohy sjednotit na BG (JPEG šum)
    px = crop.load()
    for y in range(side):
        for x in range(side):
            r, g, b = px[x, y]
            if r + g + b < 3 * 60:
                px[x, y] = BG[:3]
    logo = crop.resize((BIG, BIG), Image.LANCZOS)

    # zaoblený čtverec (průhledné rohy)
    mask = Image.new("L", (BIG, BIG), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, BIG - 1, BIG - 1), radius=int(BIG * 0.2), fill=255)
    mask = mask.filter(ImageFilter.GaussianBlur(1.2))
    icon = logo.convert("RGBA")
    icon.putalpha(mask)

    icon.resize((512, 512), Image.LANCZOS).save(OUT / "icon.png", optimize=True)
    raw_dir = OUT / "icon"
    raw_dir.mkdir(exist_ok=True)
    for n in SIZES:
        small = icon.resize((n, n), Image.LANCZOS)
        if n <= 24:
            # malé velikosti trochu doostřit, ať kostky nesplynou
            small = small.filter(ImageFilter.UnsharpMask(radius=0.6, percent=60, threshold=0))
        (raw_dir / f"{n}.rgba").write_bytes(small.tobytes())
    print("hotovo:", OUT / "icon.png", "a", ", ".join(f"{n}.rgba" for n in SIZES))


if __name__ == "__main__":
    main()
