#!/usr/bin/env python3
"""Stand-in local process for verifying a mirrord filter.

Run it under `mirrord exec` instead of the real service. It binds the service's
port, logs every request it receives with the identifier header, and answers
200 with an empty JSON object. It performs no writes, so a request that mirror
mode copies to it has no side effects, unlike running the real service locally.

    mirrord exec -f .mirrord/mirrord.json --key alice -- python3 request_recorder.py --port 8080 --header baggage

Stdlib only.
"""

import argparse
import http.server
import sys


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", type=int, required=True, help="the port the target service listens on")
    ap.add_argument("--header", action="append", default=[], help="header(s) to print with each request (repeatable)")
    args = ap.parse_args()
    names = [h.lower() for h in args.header] or ["baggage"]

    class Recorder(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def _handle(self):
            length = int(self.headers.get("content-length") or 0)
            if length:
                self.rfile.read(length)
            shown = " ".join("%s=%r" % (n, self.headers.get(n)) for n in names)
            print("RECEIVED %s %s %s" % (self.command, self.path, shown), flush=True)
            body = b"{}"
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        do_GET = do_POST = do_PUT = do_PATCH = do_DELETE = do_OPTIONS = do_HEAD = _handle

        def log_message(self, *_):
            pass

    print("recorder listening on %d, printing %s" % (args.port, ", ".join(names)), flush=True)
    try:
        http.server.ThreadingHTTPServer(("0.0.0.0", args.port), Recorder).serve_forever()
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    sys.exit(main())
