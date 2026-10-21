import hashlib
import hmac
import os

key = os.environ["WEBHOOK_KEY"].encode()
sig = hmac.new(key, b'{"event":"task.done","id":1}', hashlib.sha256).hexdigest()
print("signature", sig)
print("key-length", len(key))
