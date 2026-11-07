import http.server, json, sys

class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        body = self.rfile.read(n).decode()
        print(json.loads(body), flush=True)
        self.send_response(204)
        self.end_headers()

http.server.HTTPServer(("127.0.0.1", 8399), H).serve_forever()
