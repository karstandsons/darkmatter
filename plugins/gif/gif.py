#!/usr/bin/env python3
"""Darkmatter GIF plugin: search Giphy, attach the GIF you pick.

Needs a Giphy API key (free at developers.giphy.com): put it in
GIPHY_API_KEY, or in `key.txt` next to this file.
"""
import concurrent.futures
import json
import os
import shutil
import sys
import urllib.parse
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.environ.get("DARKMATTER_OUTPUT_DIR") or os.path.expanduser("~/.cache/darkmatter/plugins/gif")
THUMBS = os.path.join(OUT, "thumbs")
SHOWN = 16


def key():
    k = os.environ.get("GIPHY_API_KEY", "").strip()
    if not k and os.path.exists(os.path.join(HERE, "key.txt")):
        with open(os.path.join(HERE, "key.txt")) as f:
            k = f.read().strip()
    if not k:
        raise RuntimeError(f"needs a Giphy API key: set GIPHY_API_KEY or put it in {os.path.join(HERE, 'key.txt')}")
    return k


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
