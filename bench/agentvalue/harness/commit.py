#!/usr/bin/env python3
"""Commit only after a full scan of everything the commit would contain (PROTOCOL.md, Version 2).

  python3 bench/agentvalue/harness/commit.py -m "message"      # or -F message-file
  python3 bench/agentvalue/harness/commit.py --check -m "..."  # scan only, never commit

Lists the staged paths with NUL-delimited plumbing (`git diff --cached --name-status -z`, so quoted
or unusual names are read exactly), reads every staged blob in full from the index
(`git ls-files -s -z`, `git cat-file blob`), and scans text and binary content, symbolic-link
targets, renamed and copied files, the path names and the commit message for credential patterns
and for the value of any host environment variable other than HOME, USER and LOGNAME (8 characters
or more; also when split across lines or JSON-escaped). On any hit it prints the path and the kind of
hit, never the value, and refuses to commit. Otherwise it runs `git commit` with the message.
"""
import argparse
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import scan_staged


def git(repo, *argv, data=None):
    return subprocess.run(['git', *argv], cwd=repo, capture_output=True, input=data, check=True).stdout


def staged_entries(repo):
    """(status, path) for every staged change other than a deletion; renames and copies give the new path."""
    fields = git(repo, 'diff', '--cached', '--name-status', '-z', '-M').split(b'\0')
    entries, i = [], 0
    while i < len(fields) and fields[i]:
        status = fields[i].decode()
        if status[0] in 'RC':
            entries.append((status, fields[i + 2]))
            i += 3
        else:
            if status[0] != 'D':
                entries.append((status, fields[i + 1]))
            i += 2
    return entries


def blob_of(repo, path):
    """(mode, content) of a path as staged in the index."""
    line = git(repo, 'ls-files', '-s', '-z', '--', path).split(b'\0')[0]
    meta = line.split(b'\t', 1)[0].split()
    mode, sha = meta[0].decode(), meta[1].decode()
    return mode, git(repo, 'cat-file', 'blob', sha)


def scan(repo, message, environ=None):
    hits = []
    for status, raw_path in staged_entries(repo):
        name = raw_path.decode('utf-8', errors='replace')
        for hit in scan_staged.scan_blob(raw_path, environ):
            hits.append((name, f'{hit} in the path name'))
        mode, content = blob_of(repo, raw_path)
        kind = 'symbolic-link target' if mode == '120000' else 'submodule' if mode == '160000' else \
            'binary content' if b'\0' in content[:8192] else 'content'
        for hit in scan_staged.scan_blob(content, environ):
            hits.append((name, f'{hit} ({kind}{", " + ("renamed" if status[0] == "R" else "copied") if status[0] in "RC" else ""})'))
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
    if not staged_entries(args.repo) and not args.check:
        raise SystemExit('nothing staged')
    hits = scan(args.repo, message)
    for path, what in hits:
        print(f'{path!r}: {what}')
    if hits:
        print(f'refusing to commit: {len(hits)} hit(s)', file=sys.stderr)
        sys.exit(1)
    print('scan clean', file=sys.stderr)
    if args.check:
        return
    sys.exit(subprocess.run(['git', 'commit', '-F', '-'], cwd=args.repo, input=message.encode()).returncode)


if __name__ == '__main__':
    main()
