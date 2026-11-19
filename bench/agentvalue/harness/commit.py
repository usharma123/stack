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
index state it scanned: the tree id taken before the scan must be unchanged just before the commit.
"""
import argparse
import hashlib
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import scan_staged


def git(repo, *argv, data=None):
    return subprocess.run(['git', '--literal-pathspecs', *argv], cwd=repo, capture_output=True, input=data, check=True).stdout


def index_entries(repo):
    """path bytes -> (mode, object id), from one reading of the index (stage 0 entries)."""
    entries = {}
    for record in git(repo, 'ls-files', '-s', '-z').split(b'\0'):
        if not record:
            continue
        meta, _, path = record.partition(b'\t')
        mode, sha, stage = meta.split()
        if stage == b'0':
            entries[path] = (mode.decode(), sha.decode())
    return entries


def staged_changes(repo):
    """(status, path bytes) for every staged change other than a deletion; renames and copies give the new path."""
    fields = git(repo, 'diff', '--cached', '--name-status', '-z', '-M').split(b'\0')
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


def scan(repo, message, environ=None):
    """Hits as (shown path, what). Uses one reading of the index for every lookup."""
    index = index_entries(repo)
    hits, masked = [], {}
    changes = staged_changes(repo)
    for status, raw_path in changes:
        if scan_staged.scan_blob(raw_path, environ):
            masked[raw_path] = f'<path name masked: sha256 {hashlib.sha256(raw_path).hexdigest()[:12]}>'
    for status, raw_path in changes:
        shown = masked.get(raw_path) or raw_path.decode('utf-8', errors='replace')
        if raw_path in masked:
            for hit in scan_staged.scan_blob(raw_path, environ):
                hits.append((shown, f'{hit} in the path name'))
        if raw_path not in index:
            hits.append((shown, 'staged path not found in the index (cannot be scanned)'))
            continue
        mode, sha = index[raw_path]
        content = git(repo, 'cat-file', 'blob', sha) if mode != '160000' else b''
        kind = 'symbolic-link target' if mode == '120000' else 'submodule' if mode == '160000' else \
            'binary content' if b'\0' in content[:8192] else 'content'
        for hit in scan_staged.scan_blob(content, environ):
            hits.append((shown, f'{hit} ({kind}{", " + ("renamed" if status[0] == "R" else "copied") if status[0] in "RC" else ""})'))
    for hit in scan_staged.scan_blob(message.encode(), environ):
        hits.append(('(commit message)', hit))
    return hits


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument('-m', dest='message')
    group.add_argument('-F', dest='message_file')
    parser.add_argument('--repo', default='.')
    parser.add_argument('--check', action='store_true', help='scan only')
    args = parser.parse_args()
    message = args.message if args.message is not None else Path(args.message_file).read_text()
    if not staged_changes(args.repo) and not args.check:
        raise SystemExit('nothing staged')
    tree = git(args.repo, 'write-tree').strip()   # the index state that is scanned and then committed
    hits = scan(args.repo, message)
    for path, what in hits:
        print(f'{path!r}: {what}')
    if hits:
        print(f'refusing to commit: {len(hits)} hit(s)', file=sys.stderr)
        sys.exit(1)
    print('scan clean', file=sys.stderr)
    if args.check:
        return
    if git(args.repo, 'write-tree').strip() != tree:
        raise SystemExit('the index changed during the scan; nothing committed')
    sys.exit(subprocess.run(['git', 'commit', '-F', '-'], cwd=args.repo, input=message.encode()).returncode)


if __name__ == '__main__':
    main()
