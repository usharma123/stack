"""Identity probe: prints the token whatever API_PORT reaches answers with."""
import os
import sys
import urllib.request

with urllib.request.urlopen(f"http://127.0.0.1:{os.environ['API_PORT']}/identity", timeout=3) as r:
    sys.stdout.write(r.read(4096).decode())
