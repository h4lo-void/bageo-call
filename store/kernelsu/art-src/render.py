"""Render the store art for Bageo AI Call Agent.

Run on the Mac (needs Playwright's Chromium and Pillow):
    python3 render.py <output-dir>
Writes icon.png (512x512) and, for each language (en, zh-CN),
banner.<lang>.png (1280x640) and demo.<lang>.gif (420x800).
"""
import glob
import os
import pathlib
import sys

from PIL import Image
from playwright.sync_api import sync_playwright

SRC = pathlib.Path(__file__).resolve().parent
OUT = pathlib.Path(sys.argv[1]).resolve()
OUT.mkdir(parents=True, exist_ok=True)
BIRD = (SRC / "icon.svg").read_text(encoding="utf-8")
LANGS = ["en", "zh-CN"]


def save_gif(frames, durations, path):
    paletted = [f.convert("P", palette=Image.Palette.ADAPTIVE, colors=255) for f in frames]
    paletted[0].save(path, save_all=True, append_images=paletted[1:],
                     duration=durations, loop=0, optimize=True, disposal=1)


with sync_playwright() as p:
    # Use the newest Chromium already in Playwright's cache (or $CHROME), so a
    # Playwright/browser version mismatch does not force a download.
    found = sorted(glob.glob(os.path.expanduser(
        "~/Library/Caches/ms-playwright/chromium_headless_shell-*/"
        "chrome-headless-shell-mac-arm64/chrome-headless-shell")))
    exe = os.environ.get("CHROME") or (found[-1] if found else None)
    browser = p.chromium.launch(executable_path=exe) if exe else p.chromium.launch()

    page = browser.new_page(viewport={"width": 512, "height": 512})
    page.set_content(
        '<html><body style="margin:0;background:transparent">'
        + BIRD.replace("<svg ", '<svg width="512" height="512" ', 1)
        + "</body></html>"
    )
    page.screenshot(path=str(OUT / "icon.png"), omit_background=True)

    for lang in LANGS:
        page = browser.new_page(viewport={"width": 1280, "height": 640})
        page.goto((SRC / "banner.html").as_uri() + "?lang=" + lang)
        page.wait_for_timeout(400)
        page.screenshot(path=str(OUT / f"banner.{lang}.png"))

        page = browser.new_page(viewport={"width": 420, "height": 800})
        page.goto((SRC / "demo.html").as_uri() + "?lang=" + lang)
        page.wait_for_timeout(400)
        count = page.evaluate("FRAMES")
        durations = page.evaluate("DUR")
        frames = []
        for i in range(count):
            page.evaluate(f"show({i})")
            page.wait_for_timeout(60)
            shot = OUT / f"_frame{i:02d}.png"
            page.screenshot(path=str(shot))
            frames.append(Image.open(shot).convert("RGB"))
            shot.unlink()
        save_gif(frames, durations, OUT / f"demo.{lang}.gif")
        print(lang, "frames", count, "total ms", sum(durations))
    browser.close()
