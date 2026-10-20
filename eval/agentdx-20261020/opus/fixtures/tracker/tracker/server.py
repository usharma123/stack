"""Task tracker HTTP API. GET /tasks is cached in Redis."""
import http.server
import json
import os
import sys

import psycopg
import redis

from tracker.migrate import migrate

CACHE_KEY = "tasks:all"
CACHE_TTL = 300
db = psycopg.connect(os.environ["DATABASE_URL"], autocommit=False)
cache = redis.Redis.from_url(os.environ["REDIS_URL"])
TOKEN = os.environ.get("STACK_IDENTITY_API", "")


def list_tasks():
    hit = cache.get(CACHE_KEY)
    if hit is not None:
        return json.loads(hit)
    rows = db.execute("SELECT id, title, done FROM tasks ORDER BY id").fetchall()
    db.commit()
    tasks = [{"id": i, "title": t, "done": d} for i, t, d in rows]
    cache.set(CACHE_KEY, json.dumps(tasks), ex=CACHE_TTL)
    return tasks


def create_task(body):
    row = db.execute("INSERT INTO tasks (title) VALUES (%s) RETURNING id", (body["title"],)).fetchone()
    db.commit()
    return {"id": row[0], "title": body["title"], "done": False}


def complete_task(task_id):
    n = db.execute("UPDATE tasks SET done = TRUE WHERE id = %s", (task_id,)).rowcount
    db.commit()
    return n


class Handler(http.server.BaseHTTPRequestHandler):
    def reply(self, code, payload):
        body = payload if isinstance(payload, bytes) else json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            return self.reply(200, {"ok": True})
        if self.path == "/identity":
            return self.reply(200, TOKEN.encode())
        if self.path == "/tasks":
            return self.reply(200, list_tasks())
        self.reply(404, {"error": "not found"})

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(length) or b"{}")
        if self.path == "/tasks":
            return self.reply(201, create_task(body))
        parts = self.path.strip("/").split("/")
        if len(parts) == 3 and parts[0] == "tasks" and parts[2] == "done":
            return self.reply(200 if complete_task(int(parts[1])) else 404, {"id": int(parts[1])})
        self.reply(404, {"error": "not found"})

    def log_message(self, fmt, *args):
        sys.stderr.write("api: " + fmt % args + "\n")


if __name__ == "__main__":
    print("api: applied migrations:", migrate(db), flush=True)
    port = int(os.environ["PORT"])
    print(f"api: listening on {port}", flush=True)
    http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
