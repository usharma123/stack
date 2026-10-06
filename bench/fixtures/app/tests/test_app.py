"""Live checks run through each tool's environment: `uv run --frozen pytest -q`."""
import os
import sys

import pytest

from rwbapp import core

CHECKOUT = os.environ.get("RWB_CHECKOUT", "pytest")


def run(*argv):
    args = core.parse(list(argv))
    return core.COMMANDS[args.command](args)


def test_python_is_313():
    assert sys.version_info[:2] == (3, 13)


def test_urls_have_no_fallback(monkeypatch):
    monkeypatch.delenv("DATABASE_URL", raising=False)
    with pytest.raises(core.ConfigError):
        core.connect_pg()


def test_migrate_is_idempotent():
    run("migrate")
    second = run("migrate")
    assert second["applied"] == []
    assert second["current"] == [v for v, _ in core.migration_files()]


def test_crud_and_cache():
    run("mark", "--checkout", CHECKOUT)
    assert run("crud", "--checkout", CHECKOUT)["steps"] == ["create", "read", "update", "list", "delete"]
    assert run("cache", "--checkout", CHECKOUT)["sequence"] == ["miss", "hit", "miss", "hit"]
