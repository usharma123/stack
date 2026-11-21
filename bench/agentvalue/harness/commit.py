#!/usr/bin/env python3
"""Commit only after a full scan of everything the commit would contain (PROTOCOL.md, Version 2).

  python3 bench/agentvalue/harness/commit.py -m "message"      # or -F message-file
  python3 bench/agentvalue/harness/commit.py --check -m "..."  # scan only, never commit

Reads the index once (`git ls-files -s -z`) and maps every staged path, as exact bytes, to its
object id; lists the staged changes with NUL-delimited plumbing (`git diff --cached --name-status
-z`); reads each blob by its object id (`git cat-file blob <id>`), so no path is ever interpreted as
a pattern. Scans text and binary content, symbolic-link targets, renamed and copied files, the path
names and the commit message for credential patterns and for the value of any host environment
variable other than HOME, USER and LOGNAME (8 characters or more; also when split across lines or
JSON-escaped). On a hit it prints the path and the kind of hit, never the value (a path whose name
itself is a hit is masked everywhere), and refuses to commit. Otherwise it commits exactly the
tree it scanned, with `git commit-tree` and `git update-ref` (no hooks run; the index cannot change
what is committed), and prints what `git commit` would.
"""
import argparse
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import scan_staged


def git(repo, *argv, data=None):
    return subprocess.run(['git', '--literal-pathspecs', *argv], cwd=repo, capture_output=True, input=data, check=True).stdout


def head(repo):
    r = subprocess.run(['git', 'rev-parse', '--verify', '-q', 'HEAD'], cwd=repo, capture_output=True, text=True)
    return r.stdout.strip() or None


def tree_entries(repo, tree):
    """path bytes -> (mode, object id) for every blob, link and submodule of `tree`."""
    entries = {}
    for record in git(repo, 'ls-tree', '-r', '-z', tree).split(b'\0'):
        if not record:
            continue
        meta, _, path = record.partition(b'\t')
        mode, _kind, sha = meta.split()
        entries[path] = (mode.decode(), sha.decode())
    return entries


def tree_changes(repo, tree, parent):
    """(status, path bytes) for every change of `tree` against `parent` (or every path of an initial
    tree) other than a deletion; renames and copies give the new path."""
    if parent is None:
        return [('A', path) for path in tree_entries(repo, tree)]
    fields = git(repo, 'diff-tree', '-r', '-z', '--name-status', '-M', '--no-commit-id', parent, tree).split(b'\0')
    changes, i = [], 0
    while i < len(fields) and fields[i]:
        status = fields[i].decode()
        if status[0] in 'RC':
            changes.append((status, fields[i + 2]))
            i += 3
        else:
            if status[0] != 'D':
                changes.append((status, fields[i + 1]))
            i += 2
    return changes


def scan(repo, message, environ=None, tree=None):
    """Hits as (shown path, what), for every blob of `tree` (default: the index written as a tree) that
    differs from HEAD, read by object id, and for the message."""
    tree = tree or git(repo, 'write-tree').strip().decode()
    parent = head(repo)
    entries = tree_entries(repo, tree)
    changes = tree_changes(repo, tree, parent)
    hits, masked = [], {}
    for status, raw_path in changes:
        if scan_staged.scan_blob(raw_path, environ):
            masked[raw_path] = f'<path name masked: sha256 {hashlib.sha256(raw_path).hexdigest()[:12]}>'
    for status, raw_path in changes:
        shown = masked.get(raw_path) or raw_path.decode('utf-8', errors='replace')
        if raw_path in masked:
            for hit in scan_staged.scan_blob(raw_path, environ):
                hits.append((shown, f'{hit} in the path name'))
        if raw_path not in entries:
            hits.append((shown, 'path not found in the scanned tree (cannot be scanned)'))
            continue
        mode, sha = entries[raw_path]
        content = git(repo, 'cat-file', 'blob', sha) if mode != '160000' else b''
        kind = 'symbolic-link target' if mode == '120000' else 'submodule' if mode == '160000' else \
            'binary content' if b'\0' in content[:8192] else 'content'
        for hit in scan_staged.scan_blob(content, environ):
            hits.append((shown, f'{hit} ({kind}{", " + ("renamed" if status[0] == "R" else "copied") if status[0] in "RC" else ""})'))
    for hit in scan_staged.scan_blob(message.encode(), environ):
        hits.append(('(commit message)', hit))
    return hits, tree, parent, changes


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument('-m', dest='message')
    group.add_argument('-F', dest='message_file')
    parser.add_argument('--repo', default='.')
    parser.add_argument('--check', action='store_true', help='scan only')
    args = parser.parse_args()
    message = args.message if args.message is not None else Path(args.message_file).read_text()
    if not message.endswith('\n'):
        message += '\n'
    hits, tree, parent, changes = scan(args.repo, message)
    if not changes and not args.check:
        raise SystemExit('nothing to commit: the index is the same as HEAD')
    for path, what in hits:
        print(f'{path!r}: {what}')
    if hits:
        print(f'refusing to commit: {len(hits)} hit(s)', file=sys.stderr)
        sys.exit(1)
    print('scan clean', file=sys.stderr)
    if args.check:
        return
    # The commit is made from the very tree that was scanned: no hook runs, and the index cannot
    # change what is committed. Author and committer come from the repository's configuration, as
    # with `git commit`.
    with tempfile.NamedTemporaryFile('w', delete=False, prefix='commit-msg-') as handle:
        handle.write(message)
        message_file = handle.name
    try:
        argv = ['commit-tree', tree] + (['-p', parent] if parent else []) + ['-F', message_file]
        commit = git(args.repo, *argv).strip().decode()
    finally:
        os.unlink(message_file)
    git(args.repo, 'update-ref', '-m', f'commit: {message.splitlines()[0][:80]}', 'HEAD', commit, parent or '0' * 40)
    branch = subprocess.run(['git', 'symbolic-ref', '--short', '-q', 'HEAD'], cwd=args.repo, capture_output=True, text=True).stdout.strip()
    print(f"[{branch or 'detached HEAD'}{'' if parent else ' (root-commit)'} {commit[:7]}] {message.splitlines()[0]}")
    stat = git(args.repo, 'diff', '--shortstat', parent or '4b825dc642cb6eb9a060e54bf8d69288fbee4904', commit).decode().strip()
    if stat:
        print(' ' + stat)


if __name__ == '__main__':
    main()
