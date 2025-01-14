from http.server import HTTPServer, BaseHTTPRequestHandler
import time;

class ChunkedHTTPHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        # Send response headers
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()

        # Send the response in chunks
        chunks = [
            "This is the first chunk.\n"*8000,
            "This is the second chunk.\n"
        ]
        for chunk in chunks:
            time.sleep(3.0)
            # Write the chunk size in hexadecimal followed by the chunk data
            self.wfile.write(f"{len(chunk):X}\r\n".encode('utf-8'))
            self.wfile.write(chunk.encode('utf-8'))
            self.wfile.write(b"\r\n")

        # Signal the end of the response
        self.wfile.write(b"0\r\n\r\n")

if __name__ == "__main__":
    port = 80
    print(f"Starting server on port {port}")
    server = HTTPServer(("0.0.0.0", port), ChunkedHTTPHandler)
    server.serve_forever()
