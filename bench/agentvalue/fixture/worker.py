"""Runs queued jobs from Redis until stopped."""
import json
import os
import signal
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from ledger import cache, config, jobs

running = True


def stop(*_):
    global running
    running = False


signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)


class Health(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b'ok')


# Optional liveness endpoint for process supervisors that check a port.
if os.environ.get('WORKER_HEALTH_PORT'):
    health = ThreadingHTTPServer(('127.0.0.1', int(os.environ['WORKER_HEALTH_PORT'])), Health)
    threading.Thread(target=health.serve_forever, daemon=True).start()
print('ledger worker waiting for jobs', flush=True)
while running:
    item = cache.client().blpop([config.QUEUE], timeout=1)
    if item is None:
        continue
    job = json.loads(item[1])
    try:
        jobs.handle(job)
    except Exception as error:  # keep the worker alive; the job is logged and dropped
        print(f'job failed: {job!r}: {error}', file=sys.stderr, flush=True)
