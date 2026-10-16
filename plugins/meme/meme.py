#!/usr/bin/env python3
"""Darkmatter meme plugin: imgflip's popular templates, captioned on your
computer with ImageMagick. Needs `magick` (or `convert`) and the internet
for the template list and images (cached).

Search with "template | top text | bottom text"; pick a template to caption it.
Or: /meme drake | tabs | spaces
"""
import concurrent.futures
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.request

OUT = os.environ.get("DARKMATTER_OUTPUT_DIR") or os.path.expanduser("~/.cache/darkmatter/plugins/meme")
TEMPLATES = os.path.join(OUT, "templates")
LIST = os.path.join(OUT, "memes.json")
LIST_URL = "https://api.imgflip.com/get_memes"
SHOWN = 12


def fetch(url, path):
    if not os.path.exists(path):
        req = urllib.request.Request(url, headers={"User-Agent": "darkmatter-meme-plugin"})
        with urllib.request.urlopen(req, timeout=15) as r, open(path + ".part", "wb") as f:
            shutil.copyfileobj(r, f)
        os.replace(path + ".part", path)
    return path


def templates():
    # The list changes slowly: refresh it once a day.
    if not os.path.exists(LIST) or time.time() - os.path.getmtime(LIST) > 86400:
        try:
            os.remove(LIST)
        except FileNotFoundError:
            pass
        fetch(LIST_URL, LIST)
    with open(LIST) as f:
        return json.load(f)["data"]["memes"]


def image_of(meme):
    ext = os.path.splitext(meme["url"])[1] or ".jpg"
    return fetch(meme["url"], os.path.join(TEMPLATES, meme["id"] + ext))


def split(text):
    parts = [p.strip() for p in text.split("|")]
    return parts[0], parts[1] if len(parts) > 1 else "", parts[2] if len(parts) > 2 else ""


def find(name):
    q = name.lower()
    memes = templates()
    return [m for m in memes if q in m["name"].lower()] if q else memes


def caption(meme, top, bottom):
    src = image_of(meme)
    key = hashlib.sha1(f"{meme['id']}|{top}|{bottom}".encode()).hexdigest()[:12]
    slug = "".join(c if c.isalnum() else "-" for c in meme["name"].lower()).strip("-")[:40]
    out = os.path.join(OUT, f"{slug}-{key}.png")
    magick = shutil.which("magick") or shutil.which("convert")
    if not magick:
        raise RuntimeError("needs ImageMagick (magick or convert)")
    args = [magick, src, "-resize", "800x800>"]
    size = max(24, min(800, meme["width"]) // 12)
    common = ["-fill", "white", "-stroke", "black", "-strokewidth", str(max(2, size // 16)), "-pointsize", str(size)]
    font = font_path()
    if font:
        common += ["-font", font]
    for text, gravity in ((top, "north"), (bottom, "south")):
        if text:
            args += common + ["-gravity", gravity, "-annotate", "+0+10", text.upper()]
    subprocess.run(args + [out], check=True, capture_output=True)
    return out


def font_path():
    """Impact if it's installed, or fontconfig's nearest bold sans."""
    try:
        r = subprocess.run(["fc-match", "-f", "%{file}", "Impact:bold"], capture_output=True, text=True, timeout=5)
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return None
    return r.stdout if r.returncode == 0 and r.stdout else None


def answer(**fields):
    print(json.dumps(fields), flush=True)


def main():
    os.makedirs(TEMPLATES, exist_ok=True)
    request = json.loads(sys.stdin.readline())
    kind = request["type"]
    try:
        if kind == "search":
            name, top, bottom = split(request["query"])
            memes = find(name)[:SHOWN]
            with concurrent.futures.ThreadPoolExecutor(6) as pool:
                thumbs = list(pool.map(lambda m: safe(image_of, m), memes))
            items = [
                {"title": m["name"], "thumb": t, "value": json.dumps([m["id"], top, bottom])}
                for m, t in zip(memes, thumbs)
            ]
            answer(items=items)
        elif kind == "pick":
            meme_id, top, bottom = json.loads(request["value"])
            meme = next(m for m in templates() if m["id"] == meme_id)
            answer(attach=caption(meme, top, bottom))
        elif kind == "command":
            name, top, bottom = split(request["args"])
            found = find(name)
            if not name or not found:
                answer(error=f"no template matches {name!r}: try the + menu's Meme picker")
                return
            answer(attach=caption(found[0], top, bottom))
    except Exception as e:  # Tell Darkmatter what went wrong, briefly.
        answer(error=str(e))


def safe(f, *args):
    try:
        return f(*args)
    except Exception:
        return None


if __name__ == "__main__":
    main()
