#!/usr/bin/env python3
"""Check moved Git tags with an empty cache using the published native binary.

Usage: python3 eval/harness/stack-lock.py STACK_BINARY RESULTS_DIR
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

binary, results = [Path(p).resolve() for p in sys.argv[1:]]
results.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix="stack-lock-") as temp:
    root = Path(temp)
    bundle = root / "bundle"
    bundle.mkdir()
    env = dict(os.environ, STACK_CACHE_DIR=str(root / "cache1"), STACK_STATE_DIR=str(root / "state"))
    def run(cwd, *args):
        return subprocess.check_output(args, cwd=cwd, env=env, stderr=subprocess.PIPE, text=True)
    def compile_to(app, label, *flags):
        output = run(app, str(binary), "compile", "--json", *flags)
        (results / (label + ".json")).write_text(output)
        data = json.loads(output)
        assert data["ok"]
        return data["data"]["bundles"][0]
    run(bundle, "git", "init", "-q")
    run(bundle, "git", "config", "user.email", "benchmark@example.invalid")
    run(bundle, "git", "config", "user.name", "Stack benchmark")
    (bundle / "bundle.toml").write_text('[bundle]\nname = "pin-probe"\n[env]\nMARKER = "v1"\n')
    (bundle / "supporting-file.txt").write_text("v1\n")
    run(bundle, "git", "add", ".")
    run(bundle, "git", "commit", "-qm", "v1")
    run(bundle, "git", "tag", "v1")
    app = root / "app"
    app.mkdir()
    (app / "stack.toml").write_text(f'[[use]]\nbundle = "git+file://{bundle}?ref=v1"\n')
    first = compile_to(app, "first")
    (bundle / "bundle.toml").write_text('[bundle]\nname = "pin-probe"\n[env]\nMARKER = "v2"\n')
    (bundle / "supporting-file.txt").write_text("v2\n")
    run(bundle, "git", "add", ".")
    run(bundle, "git", "commit", "-qm", "v2")
    run(bundle, "git", "tag", "-f", "v1")
    fresh = root / "fresh"
    fresh.mkdir()
    for filename in ("stack.toml", "stack.lock"):
        shutil.copy(app / filename, fresh / filename)
    env["STACK_CACHE_DIR"] = str(root / "cache2")
    pinned = compile_to(fresh, "fresh-cache-locked", "--locked")
    assert pinned["commit"] == first["commit"] and pinned["content_hash"] == first["content_hash"]
    updated = compile_to(fresh, "updated", "--update")
    assert updated["commit"] != first["commit"] and updated["content_hash"] != first["content_hash"]
    assert updated["moved_from"] == first["commit"]
    (results / "summary.json").write_text(json.dumps({"ok": True, "fresh_cache_preserved_pin": True, "explicit_update_reported_move": True}, indent=2) + "\n")
    print("Moved tag preserved across fresh cache; explicit update reported new commit.")
