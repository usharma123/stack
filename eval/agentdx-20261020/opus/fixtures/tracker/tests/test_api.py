"""Integration tests against this checkout's running API, Postgres and Redis."""
import json
import os
import urllib.request

API = os.environ["API_URL"]


def call(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(API + path, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=5) as r:
        return r.status, json.loads(r.read())


def test_health():
    assert call("GET", "/health") == (200, {"ok": True})


def test_create_then_list():
    _, t = call("POST", "/tasks", {"title": "write tests"})
    _, tasks = call("GET", "/tasks")
    assert t["id"] in [x["id"] for x in tasks]
