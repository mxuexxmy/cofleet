import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        healthy = Path('/app/health.txt').read_text().strip() == 'ok'
        body = json.dumps({'app': 'cofleet-release-demo', 'version': Path('/app/version.txt').read_text().strip()}).encode()
        self.send_response(503 if self.path == '/health' and not healthy else 200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
ThreadingHTTPServer(('0.0.0.0', 8080), Handler).serve_forever()
