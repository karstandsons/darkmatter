#!/usr/bin/env python3
"""Darkmatter GIF plugin: search Giphy, attach the GIF you pick.

Needs a Giphy API key (free at developers.giphy.com). Give it once with
`/gif key <your key>` (kept in Darkmatter's data folder), or set `api_key`
under `[plugins.gif]` in config.toml, or GIPHY_API_KEY.
"""
import concurrent.futures
import json
import os
import shutil
import sys
import urllib.error
import urllib.parse
import urllib.request

OUT = os.environ.get("DARKMATTER_OUTPUT_DIR") or os.path.expanduser("~/.cache/darkmatter/plugins/gif")
DATA = os.environ.get("DARKMATTER_DATA_DIR") or os.path.expanduser("~/.local/share/darkmatter/plugins/gif")
KEY_FILE = os.path.join(DATA, "api_key")
SIGN_UP = "https://developers.giphy.com/dashboard/?create=true"
THUMBS = os.path.join(OUT, "thumbs")
SHOWN = 16


CONFIG = {}


def key():
    k = str(CONFIG.get("api_key", "")).strip() or os.environ.get("GIPHY_API_KEY", "").strip()
    if not k and os.path.exists(KEY_FILE):
        with open(KEY_FILE) as f:
            k = f.read().strip()
    if not k:
        raise RuntimeError(f"needs a Giphy API key: get a free one at {SIGN_UP}, then send /gif key <your key>")
    return k


def save_key(k):
    os.makedirs(DATA, exist_ok=True)
    fd = os.open(KEY_FILE, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write(k.strip() + "\n")


def get(url, path=None):
    req = urllib.request.Request(url, headers={"User-Agent": "darkmatter-gif-plugin"})
    with urllib.request.urlopen(req, timeout=15) as r:
        if path is None:
            return json.load(r)
        with open(path + ".part", "wb") as f:
            shutil.copyfileobj(r, f)
    os.replace(path + ".part", path)
    return path


def search(query, limit):
    params = {"api_key": key(), "limit": limit, "rating": "pg-13"}
    if query.strip():
        endpoint = "search"
        params["q"] = query
    else:
        endpoint = "trending"
    return get(f"https://api.giphy.com/v1/gifs/{endpoint}?" + urllib.parse.urlencode(params))["data"]


def thumb(gif):
    """A still frame, small, for the picker."""
    path = os.path.join(THUMBS, gif["id"] + ".gif")
    if not os.path.exists(path):
        get(gif["images"]["fixed_height_small_still"]["url"], path)
    return path


def download(gif_id):
    data = get(f"https://api.giphy.com/v1/gifs/{urllib.parse.quote(gif_id)}?" + urllib.parse.urlencode({"api_key": key()}))["data"]
    slug = (data.get("slug") or gif_id)[:60]
    path = os.path.join(OUT, slug + ".gif")
    if not os.path.exists(path):
        get(data["images"]["original"]["url"], path)
    return path


def answer(**fields):
    print(json.dumps(fields), flush=True)


def main():
    os.makedirs(THUMBS, exist_ok=True)
    request = json.loads(sys.stdin.readline())
    CONFIG.update(request.get("config") or {})
    kind = request["type"]
    try:
        if kind == "search":
            gifs = search(request["query"], SHOWN)

            def still(g):
                try:
                    return thumb(g)
                except Exception:
                    return None

            with concurrent.futures.ThreadPoolExecutor(8) as pool:
                thumbs = list(pool.map(still, gifs))
            answer(items=[{"title": g.get("title") or g["id"], "thumb": t, "value": g["id"]} for g, t in zip(gifs, thumbs)])
        elif kind == "pick":
            answer(attach=download(request["value"]))
        elif kind == "command" and request["args"].split(" ", 1)[0] == "key":
            k = request["args"][4:].strip()
            if not k:
                answer(error=f"send /gif key <your key> (free at {SIGN_UP})")
                return
            # Check it works before keeping it.
            CONFIG["api_key"] = k
            try:
                search("hello", 1)
            except urllib.error.HTTPError as e:
                if e.code in (401, 403):
                    answer(error="Giphy didn't accept that key; nothing was saved")
                    return
                raise
            save_key(k)
            answer(notice="Saved your Giphy key. Try the + menu's GIF picker.")
        elif kind == "command":
            gifs = search(request["args"], 1)
            if not gifs:
                answer(error=f"no GIFs for {request['args']!r}")
                return
            answer(attach=download(gifs[0]["id"]))
    except Exception as e:
        answer(error=str(e))


if __name__ == "__main__":
    main()
