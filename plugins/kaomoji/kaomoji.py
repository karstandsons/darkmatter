#!/usr/bin/env python3
"""A Darkmatter plugin with no dependencies: a picker and a slash command.
Reads one JSON request on stdin, prints JSON answers on stdout."""
import json
import sys

FACES = {
    "shrug": r"¯\_(ツ)_/¯",
    "table flip": "(╯°□°)╯︵ ┻━┻",
    "put the table back": "┬─┬ノ( º _ ºノ)",
    "happy": "(＾▽＾)",
    "excited": "ヽ(°〇°)ﾉ",
    "lenny": "( ͡° ͜ʖ ͡°)",
    "disapproval": "ಠ_ಠ",
    "sparkles": "(ﾉ◕ヮ◕)ﾉ*:･ﾟ✧",
    "bear": "ʕ•ᴥ•ʔ",
    "cool": "(⌐■_■)",
    "cry": "(ಥ﹏ಥ)",
    "hug": "(っ˘̩╭╮˘̩)っ",
    "wave": "( ﾟ▽ﾟ)/",
    "fight me": "(ง'̀-'́)ง",
}


def matches(query):
    q = query.strip().lower()
    return [(name, face) for name, face in FACES.items() if q in name]


def main():
    request = json.loads(sys.stdin.readline())
    kind = request["type"]
    if kind == "search":
        items = [{"title": face, "description": name, "value": face} for name, face in matches(request["query"])]
        print(json.dumps({"items": items}))
    elif kind == "pick":
        print(json.dumps({"insert": request["value"]}))
    elif kind == "command":
        found = matches(request["args"])
        if found:
            print(json.dumps({"insert": found[0][1]}))
        else:
            print(json.dumps({"error": f"no kaomoji called {request['args']!r}"}))


if __name__ == "__main__":
    main()
