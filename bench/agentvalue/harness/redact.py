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


def scrub(text):
    if not isinstance(text, str):
        return text
    text = SECRET_KEY.sub(lambda m: f'{m[1]}=<redacted>', text)
    text = SECRET_JSON.sub(lambda m: f'{m[1]}{m[2]}<redacted>{m[4]}', text)
    return MISE_DIFF.sub(lambda m: f'{m[1]}=<redacted>', text)


def scrub_json(value):
    if isinstance(value, str):
        return scrub(value)
    if isinstance(value, list):
        return [scrub_json(v) for v in value]
    if isinstance(value, dict):
        return {k: scrub_json(v) for k, v in value.items()}
    return value


def scrub_file(path):
    path = Path(path)
    raw = path.read_text(errors='replace')
    try:
        new = json.dumps(scrub_json(json.loads(raw)), indent=2)
    except ValueError:
        new = '\n'.join(scrub(line) for line in raw.split('\n'))
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
