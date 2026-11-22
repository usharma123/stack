"""Remove secret-looking values from anything the harness records.

Command output can carry the host's environment (for example `env` run by an agent or by the
harness). Every `KEY=value` whose key looks like a credential has its value replaced, and the
whole of mise's encoded environment diff is dropped. Used on every recorded stdout/stderr and
run over retained result files with `python3 redact.py <dir>...`.
"""
import json
from pathlib import Path
import re
import sys

KEYWORDS = r'(?:TOKEN|SECRET|PASSWORD|PASSWD|API_KEY|APIKEY|ACCESS_KEY|PRIVATE_KEY|AUTH|COOKIE|CREDENTIAL|SESSION_KEY)'
SECRET_KEY = re.compile(r'\b([A-Za-z0-9_]*' + KEYWORDS + r'[A-Za-z0-9_]*)=("[^"]*"|\'[^\']*\'|[^\s"\'\\]+)', re.I)
# JSON and escaped JSON: "SOME_TOKEN": "value"  or  \"SOME_TOKEN\": \"value\"
SECRET_JSON = re.compile(r'(\\?"[A-Za-z0-9_]*' + KEYWORDS + r'[A-Za-z0-9_]*\\?"\s*:\s*)(\\?")((?:[^"\\]|\\(?!"))*)(\\?")', re.I)
MISE_DIFF = re.compile(r'\b(__MISE_[A-Z_]*)=[^\s"\'\\]+')
# Credential-bearing HTTP headers: the whole value is redacted (any scheme, every parameter of a
# Digest header), header names case-insensitive, in four forms: `-H 'Name: value'`, `-H "Name: value"`
# (also JSON-escaped, `-H \"Name: value\"`), JSON `"Name": "value"`, and a bare `Name: value` line.
HEADER_NAMES = r'(?:Proxy-Authorization|Authorization|X-API-Key|X-Auth-Token|Set-Cookie|Cookie)'
HEADER_SQ = re.compile(r"(-H\s*'\s*" + HEADER_NAMES + r"\s*:\s*)[^']*(')", re.I)
HEADER_DQ = re.compile(r'(-H\s*"\s*' + HEADER_NAMES + r'\s*:\s*)(?:[^"\\]|\\.)*(")', re.I)
HEADER_EDQ = re.compile(r'(-H\s*\\"\s*' + HEADER_NAMES + r'\s*:\s*)(?:[^"\\]|\\(?!")[\s\S])*(\\")', re.I)
HEADER_JSON = re.compile(r'("' + HEADER_NAMES + r'"\s*:\s*")((?:[^"\\]|\\.)*)(")', re.I)
HEADER_EJSON = re.compile(r'(\\"' + HEADER_NAMES + r'\\"\s*:\s*\\")((?:[^"\\]|\\(?!")[\s\S])*)(\\")', re.I)
# A header counts as already scrubbed only when `<redacted>` is its entire value: followed by the end
# of the value (end of text or line, a closing quote, or a JSON \n / \r escape). A value that was
# redacted only in part (`<redacted>, response="..."`) is redacted in full.
DONE = r'[ \t]*<redacted>[ \t]*(?:\Z|[\r\n\'"]|\\["nr])'
BEARER = re.compile(r'\b(Bearer\s+)(?!<redacted>)([A-Za-z0-9._~+/=-]{8,})', re.I)


HEADER_START = re.compile(r'(\b' + HEADER_NAMES + r'\s*:)(?!' + DONE + r')[ \t]*', re.I)
UNESCAPED_QUOTE = re.compile(r'(?<!\\)(?:\\\\)*"')
JSON_VALUE = re.compile(r'(?:\\(?![nr])[\s\S]|[^\\\r\n"])*')
LINE_VALUE = re.compile(r'[^\r\n]*')


def _bare_headers(line):
    """Redact every bare credential header on one line. Its extent is decided on that line alone:
    inside a JSON string literal of the line (an odd number of unescaped quotes before it) the value
    ends at a \\n / \\r escape or an unescaped quote; anywhere else it runs to the end of the line,
    quoted Digest parameters included, whatever other lines look like."""
    out, i = [], 0
    while True:
        m = HEADER_START.search(line, i)
        if not m:
            out.append(line[i:])
            return ''.join(out)
        inside = len(UNESCAPED_QUOTE.findall(line, 0, m.start())) % 2 == 1
        value = (JSON_VALUE if inside else LINE_VALUE).match(line, m.end())
        if value.end() == m.end():          # nothing after the name
            out.append(line[i:m.end()])
        else:
            out.append(line[i:m.start()] + m[1] + ' <redacted>')
        i = max(value.end(), m.end())
        if i == m.start():
            i += 1


def scrub(text):
    if not isinstance(text, str):
        return text
    stripped = text.strip()
    if stripped[:1] in ('{', '[', '"') and len(stripped) > 1:
        # Text that is itself JSON (a command's JSON output, a JSON-encoded string): scrubbed as parsed
        # JSON, recursively, so escaped and doubly encoded values are read exactly.
        try:
            parsed = json.loads(stripped)
        except ValueError:
            parsed = None
        if isinstance(parsed, (dict, list, str)) and parsed != stripped:
            new = json.dumps(scrub_json(parsed), ensure_ascii=False)
            return new if new != json.dumps(parsed, ensure_ascii=False) else text
    text = SECRET_KEY.sub(lambda m: f'{m[1]}=<redacted>', text)
    text = SECRET_JSON.sub(lambda m: f'{m[1]}{m[2]}<redacted>{m[4]}', text)
    text = HEADER_SQ.sub(lambda m: f'{m[1]}<redacted>{m[2]}', text)
    text = HEADER_EDQ.sub(lambda m: f'{m[1]}<redacted>{m[2]}', text)
    text = HEADER_DQ.sub(lambda m: f'{m[1]}<redacted>{m[2]}', text)
    text = HEADER_EJSON.sub(lambda m: f'{m[1]}<redacted>{m[3]}', text)
    text = HEADER_JSON.sub(lambda m: f'{m[1]}<redacted>{m[3]}', text)
    text = ''.join(_bare_headers(line) for line in re.split(r'(\r\n|\n|\r)', text))
    text = BEARER.sub(lambda m: f'{m[1]}<redacted>', text)
    return MISE_DIFF.sub(lambda m: f'{m[1]}=<redacted>', text)


HEADER_KEY = re.compile(r'^\s*' + HEADER_NAMES + r'\s*$', re.I)
SECRET_NAME = re.compile(r'^[A-Za-z0-9_.-]*' + KEYWORDS + r'[A-Za-z0-9_.-]*$', re.I)
# Keys that look credential-like but are counters or names (token counts in usage, the CLI's own
# identity tokens count), never a secret value.
NOT_SECRET = re.compile(r'tokens$|_tokens_|tokens_details|identity_tokens|^(?:input|output)_tokens', re.I)


def scrub_json(value):
    """Scrub a parsed JSON value: every string, and every string held under a credential-like key at
    any depth (`{"SOME_TOKEN": "..."}`)."""
    if isinstance(value, str):
        return scrub(value)
    if isinstance(value, list):
        return [scrub_json(v) for v in value]
    if isinstance(value, dict):
        return {k: (redact_all(v) if isinstance(k, str) and ((SECRET_NAME.match(k) and not NOT_SECRET.search(k)) or HEADER_KEY.match(k))
                    else scrub_json(v)) for k, v in value.items()}
    return value


def redact_all(value):
    """Everything held under a credential-like key: every string, in lists and nested objects too."""
    if isinstance(value, str):
        return '<redacted>'
    if isinstance(value, list):
        return [redact_all(v) for v in value]
    if isinstance(value, dict):
        return {k: redact_all(v) for k, v in value.items()}
    return value


def scrub_text(raw, whole=False):
    """Scrub a text file's content: with `whole` (a .json file) the document as parsed JSON; otherwise
    line by line, each line parsed when it is JSON (traces), so the line count never changes."""
    if whole:
        try:
            return json.dumps(scrub_json(json.loads(raw)), indent=2)
        except ValueError:
            pass
    out = []
    for line in raw.split('\n'):
        try:
            parsed = json.loads(line)
            if isinstance(parsed, (dict, list)):
                new = json.dumps(scrub_json(parsed))
                out.append(new if new != json.dumps(parsed) else line)
                continue
        except ValueError:
            pass
        out.append(scrub(line))
    return '\n'.join(out)


def scrub_file(path):
    path = Path(path)
    raw = path.read_text(errors='replace')
    new = scrub_text(raw, whole=path.suffix == '.json')
    if new != raw:
        path.write_text(new)
        return True
    return False


if __name__ == '__main__':
    changed = 0
    for root in sys.argv[1:]:
        for path in sorted(Path(root).rglob('*')):
            if path.is_file() and path.suffix in {'.json', '.jsonl', '.txt', '.log', '.patch'} and 'submitted-tree' not in path.parts:
                changed += scrub_file(path)
    print(f'redacted {changed} files')
