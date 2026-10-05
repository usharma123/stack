"""Prints the identity of whatever WEB_PORT reaches."""
import os
import sys
import urllib.request

with urllib.request.urlopen(f"http://127.0.0.1:{os.environ['WEB_PORT']}/identity", timeout=3) as r:
    sys.stdout.write(r.read(4096).decode())
