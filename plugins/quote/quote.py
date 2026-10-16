#!/usr/bin/env python3
"""A Darkmatter plugin with a message action: quote the message in the box."""
import json
import sys

request = json.loads(sys.stdin.readline())
if request["type"] == "action":
    post = request["post"]
    quoted = "\n".join("> " + line for line in post["message"].splitlines()) or ">"
    # The quote goes above whatever is already in the box.
    print(json.dumps({"replace": f"{quoted}\n> — @{post['user']}\n\n" + request["context"]["draft"]}))
