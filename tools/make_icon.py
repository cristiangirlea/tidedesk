"""Renders the TideDesk icon: a screen with a wave on an ocean gradient.

The wave reads both as a tide and as a sound waveform. Drawn at 4x and
downsampled for clean anti-aliasing. Outputs:
  assets/tidedesk.ico        multi-size Windows icon (16-256 px)
  assets/tidedesk-256.png    for README / store pages
  assets/tidedesk-64.rgba    raw RGBA for window and tray icons at runtime
Run: python tools/make_icon.py   (needs Pillow)
"""
import math
from pathlib import Path

from PIL import Image, ImageDraw

S = 1024  # master size
SS = 4    # supersampling
N = S * SS
OUT = Path(__file__).resolve().parent.parent / "assets"


def lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(len(a)))


def render():
    # Background gradient, top-left deep blue to bottom-right teal.
    top, bottom = (14, 42, 110), (18, 170, 160)
    grad = Image.new("RGBA", (N, N))
    px = grad.load()
    step = SS * 4
    for y in range(0, N, step):
        for x in range(0, N, step):
            c = lerp(top, bottom, (x + y) / (2 * N)) + (255,)
            for yy in range(y, min(y + step, N)):
                for xx in range(x, min(x + step, N)):
                    px[xx, yy] = c

    mask = Image.new("L", (N, N), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (int(N * 0.04), int(N * 0.04), int(N * 0.96), int(N * 0.96)),
        radius=int(N * 0.22), fill=255)
    img = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    img.paste(grad, (0, 0), mask)

    d = ImageDraw.Draw(img)
    white = (255, 255, 255, 255)
    w = int(N * 0.055)  # stroke width

    # Monitor.
    l, t, r, b = N * 0.20, N * 0.23, N * 0.80, N * 0.66
    d.rounded_rectangle((l, t, r, b), radius=int(N * 0.06), outline=white, width=w)
    # Stand.
    d.rounded_rectangle((N * 0.455, b - w / 2, N * 0.545, N * 0.76), radius=int(w * 0.3), fill=white)
    d.rounded_rectangle((N * 0.34, N * 0.735, N * 0.66, N * 0.795), radius=int(N * 0.03), fill=white)

    # Wave inside the screen.
    pts = []
    x0, x1 = l + N * 0.10, r - N * 0.10
    mid, amp = (t + b) / 2 + N * 0.01, N * 0.065
    for i in range(0, 801):
        u = i / 800
        pts.append((x0 + (x1 - x0) * u, mid - amp * math.sin(u * 2 * math.pi * 1.25 + 0.35)))
    # Stamp discs along a dense path: PIL's thick polylines leave notches at joints.
    rr = w * 0.95 / 2
    for i in range(len(pts) - 1):
        (ax, ay), (bx, by) = pts[i], pts[i + 1]
        n = max(1, int(math.hypot(bx - ax, by - ay) / 4))
        for k in range(n):
            x, y = ax + (bx - ax) * k / n, ay + (by - ay) * k / n
            d.ellipse((x - rr, y - rr, x + rr, y + rr), fill=white)
    ex, ey = pts[-1]
    d.ellipse((ex - rr, ey - rr, ex + rr, ey + rr), fill=white)

    return img.resize((S, S), Image.LANCZOS)


def main():
    OUT.mkdir(exist_ok=True)
    master = render()
    master.resize((256, 256), Image.LANCZOS).save(OUT / "tidedesk-256.png")
    master.save(OUT / "tidedesk.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    (OUT / "tidedesk-64.rgba").write_bytes(master.resize((64, 64), Image.LANCZOS).tobytes())
    print("icons written to", OUT)


if __name__ == "__main__":
    main()
