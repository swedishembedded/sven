#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements agent trajectory capture for its clients. If
# your team needs expertise in turning agent runs into training data then you
# can procure our services by sending an email to info@swedishembedded.com.
"""A recording pass-through proxy for the capture end-to-end test.

Forwards every request to the upstream model server unchanged and appends one
JSON line per exchange (request body, response status, raw response body) to
the log, so the test can compare what the server said with what the agent
recorded. Streaming responses are relayed as they arrive.

usage: capture_proxy.py --upstream http://127.0.0.1:PORT --log FILE --ready-file FILE
"""
import argparse
import http.client
import http.server
import json
import threading
import urllib.parse

LOCK = threading.Lock()


def make_handler(upstream, log_path):
    target = urllib.parse.urlparse(upstream)

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def _forward(self):
            length = int(self.headers.get("Content-Length") or 0)
            body = self.rfile.read(length) if length else b""
            conn = http.client.HTTPConnection(target.hostname, target.port, timeout=600)
            headers = {k: v for k, v in self.headers.items() if k.lower() not in ("host", "connection")}
            conn.request(self.command, self.path, body=body, headers=headers)
            resp = conn.getresponse()
            self.send_response(resp.status)
            for k, v in resp.getheaders():
                if k.lower() not in ("transfer-encoding", "connection", "content-length"):
                    self.send_header(k, v)
            self.send_header("Connection", "close")
            self.end_headers()
            received = bytearray()
            while True:
                chunk = resp.read1(65536) if hasattr(resp, "read1") else resp.read(65536)
                if not chunk:
                    break
                received += chunk
                self.wfile.write(chunk)
                self.wfile.flush()
            self.close_connection = True
            conn.close()
            if self.command == "POST":
                with LOCK, open(log_path, "a") as log:
                    log.write(json.dumps({
                        "path": self.path,
                        "request": json.loads(body or b"null"),
                        "status": resp.status,
                        "response": received.decode("utf-8", "replace"),
                    }) + "\n")

        do_GET = _forward
        do_POST = _forward

    return Handler


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--upstream", required=True)
    ap.add_argument("--log", required=True)
    ap.add_argument("--ready-file", required=True)
    ap.add_argument("--port", type=int, default=0)
    args = ap.parse_args()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", args.port), make_handler(args.upstream, args.log))
    with open(args.ready_file, "w") as f:
        f.write(str(server.server_address[1]))
    server.serve_forever()


if __name__ == "__main__":
    main()
