#!/usr/bin/env python3
"""Scan what is about to be committed for credentials (PROTOCOL.md, Version 2, "Validity, added rules").

  python3 bench/agentvalue/harness/scan_staged.py              # what is staged in git (added lines, added files)
  python3 bench/agentvalue/harness/scan_staged.py --dir PATH   # a directory, before it is staged

Two kinds of hit:
- a credential pattern (private key block, cloud and API key formats, a JWT, or a secret-named
  variable assigned a long literal value);
- the value of any host environment variable other than HOME, USER and LOGNAME, when the value is
  8 characters or more, also when split across lines or JSON-escaped. A value that names an
  existing file or directory on this host is not a credential and is skipped.

Every commit goes through commit.py, which scans every staged blob in full with this module.

Exits 1 with file and line for every hit, naming the pattern or the variable, never the value.
"""
import argparse
import os
from pathlib import Path
import re
import subprocess
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

PATTERNS = {
    'private key block': re.compile(r'-----BEGIN [A-Z ]*PRIVATE KEY-----'),
    'AWS access key id': re.compile(r'\b(?:AKIA|ASIA)[0-9A-Z]{16}\b'),
    'GitHub token': re.compile(r'\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})\b'),
    'Anthropic API key': re.compile(r'\bsk-ant-[A-Za-z0-9_-]{20,}'),
    'OpenAI-style API key': re.compile(r'\bsk-(?:proj-)?[A-Za-z0-9]{32,}\b'),
    'Slack token': re.compile(r'\bxox[abposr]-[A-Za-z0-9-]{10,}'),
    'Google API key': re.compile(r'\bAIza[0-9A-Za-z_-]{35}\b'),
    'JSON web token': re.compile(r'\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}'),
    # A secret-named variable given a long literal (not a reference such as "$X", not redacted).
    'secret-named assignment': re.compile(
        r'\b[A-Za-z0-9_]*(?:TOKEN|SECRET|PASSWORD|PASSWD|API_KEY|APIKEY|ACCESS_KEY|PRIVATE_KEY|CREDENTIAL|SESSION_KEY)[A-Za-z0-9_]*'
        r'\\?["\']?\s*[=:]\s*\\?["\']?(?![$<{])([A-Za-z0-9+/_.=-]{20,})', re.I),
}


HOST_EXEMPT = ('HOME', 'USER', 'LOGNAME')   # the only host values that may appear (they are in every path)


def host_values(environ=None):
    """(name, value) of host variables other than HOME, USER and LOGNAME whose value could be a
    credential: 8 characters or more, not a path that exists on this host, not equal to an exempt
    variable's value."""
    environ = os.environ if environ is None else environ
    exempt_values = {environ[k] for k in HOST_EXEMPT if k in environ}
    found = []
    for name, value in environ.items():
        if name in HOST_EXEMPT or len(value) < 8 or value in exempt_values:
            continue
        if value.startswith('/') and os.path.exists(value):
            continue  # a path on this host, not a credential
        found.append((name, value))
    return sorted(found, key=lambda item: -len(item[1]))


JSON_ESCAPE = re.compile(r'\\u([0-9a-fA-F]{4})|\\(["\\/bfnrt])')
ESCAPES = {'"': '"', '\\': '\\', '/': '/', 'b': '\b', 'f': '\f', 'n': '\n', 'r': '\r', 't': '\t'}


def json_decoded(text):
    r"""JSON string escapes decoded (\uXXXX, \n, \", \\, \/ ...), wherever they appear."""
    def one(m):
        if m.group(1):
            try:
                return chr(int(m.group(1), 16))
            except ValueError:
                return m.group(0)
        return ESCAPES[m.group(2)]
    return JSON_ESCAPE.sub(one, text)


def variants(text):
    r"""The text as written; with line breaks and line continuations removed; with JSON escapes
    decoded; and both. So a value split across lines or escaped in a JSON string is still found
    (host values only; credential patterns run on every variant too)."""
    joined = re.sub(r'\\\r?\n|\r?\n|\r', '', text)
    decoded = json_decoded(text)
    return {text, joined, decoded, re.sub(r'\r?\n|\r', '', decoded), json_decoded(joined)}


def scanner(environ=None):
    values = host_values(environ)
    combined = re.compile('|'.join(f'(?P<v{i}>{re.escape(v)})' for i, (_, v) in enumerate(values))) if values else None

    def scan(text):
        hits = [name for name, pattern in PATTERNS.items() if any(pattern.search(v) for v in variants(text))]
        if combined:
            for variant in variants(text):
                for match in combined.finditer(variant):
                    hit = f'value of host variable {values[int(match.lastgroup[1:])][0]}'
                    if hit not in hits:
                        hits.append(hit)
        return hits
    return scan


def staged_lines(repo):
    """(path, line number, text) of every added line in the index (added files are all added lines)."""
    diff = subprocess.run(['git', 'diff', '--cached', '-U0', '--no-color', '--no-ext-diff'], cwd=repo, capture_output=True, check=True)
    path, number = None, 0
    for raw in diff.stdout.decode('utf-8', errors='replace').split('\n'):
        if raw.startswith('+++ '):
            path = raw[6:] if raw.startswith('+++ b/') else None
        elif raw.startswith('@@'):
            match = re.search(r'\+(\d+)', raw)
            number = int(match[1]) if match else 0
        elif raw.startswith('+') and path:
            yield path, number, raw[1:]
            number += 1


def directory_lines(root):
    for path in sorted(Path(root).rglob('*')):
        if not path.is_file() or path.is_symlink() or any(p in ('.git', '__pycache__', '.venv') for p in path.parts):
            continue
        data = path.read_bytes()
        if b'\0' in data[:8192]:
            continue  # binary
        for number, line in enumerate(data.decode('utf-8', errors='replace').split('\n'), start=1):
            yield str(path), number, line


def scan_lines(lines, environ=None):
    scan = scanner(environ)
    return [(path, number, hit) for path, number, text in lines for hit in scan(text)]


def scan_directory(root, environ=None):
    return scan_lines(directory_lines(root), environ)


def scan_blob(data, environ=None):
    """Hits in a whole blob, text or binary (decoded byte for byte, so ASCII secrets inside binary
    data are found too)."""
    scan = scanner(environ)
    hits = []
    for text in (data.decode('utf-8', errors='replace'), data.decode('latin-1')):
        for hit in scan(text):
            if hit not in hits:
                hits.append(hit)
    return hits


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--dir', help='scan this directory instead of the git index')
    parser.add_argument('--repo', default='.', help='the repository whose index is scanned (default: the current directory)')
    args = parser.parse_args()
    hits = scan_directory(args.dir) if args.dir else scan_lines(staged_lines(args.repo))
    for path, number, what in hits:
        print(f'{path}:{number}: {what}')
    print(f'{len(hits)} hit(s)' if hits else 'no credential found', file=sys.stderr)
    sys.exit(1 if hits else 0)


if __name__ == '__main__':
    main()
