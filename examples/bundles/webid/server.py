"""Answers GET /identity with this instance's token; everything else is 404."""
import http.server
import os

TOKEN = os.environ["STACK_IDENTITY_WEB"].encode()


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = TOKEN if self.path == "/identity" else b"not found"
        self.send_response(200 if self.path == "/identity" else 404)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


http.server.ThreadingHTTPServer(("127.0.0.1", int(os.environ["PORT"])), Handler).serve_forever()
