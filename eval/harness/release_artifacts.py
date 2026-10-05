"""Host artifact selection for the historical published-release runners."""
import hashlib
import json
from pathlib import Path
import platform


def host_target():
    systems = {'Darwin': 'darwin', 'Linux': 'linux'}
    machines = {'arm64': 'arm64', 'aarch64': 'arm64', 'x86_64': 'x64', 'AMD64': 'x64'}
    try:
        return systems[platform.system()] + '-' + machines[platform.machine()]
    except KeyError as error:
        raise RuntimeError(f'unsupported host: {platform.system()} {platform.machine()}') from error


def released_binary(base, target=None):
    target = target or host_target()
    info = json.loads((base / 'package/package/build-info.json').read_text())
    binary = base / 'package/package/binaries' / target / 'stack'
    if hashlib.sha256(binary.read_bytes()).hexdigest() != info['hashes'][target]:
        raise RuntimeError(f'published artifact hash mismatch: {target}')
    return binary, info


def host_tools(base):
    return Path(base) / 'bin' / ('mac' if platform.system() == 'Darwin' else 'linux')
