"""Runs queued jobs from Redis until stopped."""
import json
import signal
import sys

from ledger import cache, config, jobs

running = True


def stop(*_):
    global running
    running = False


signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)
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
