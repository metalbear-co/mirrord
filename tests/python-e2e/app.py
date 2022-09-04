from os import getpid, kill
from signal import SIGTERM
import time
from flask import Flask
import logging
import sys
import threading

log = logging.getLogger("werkzeug")
log.disabled = True

cli = sys.modules["flask.cli"]

cli.show_server_banner = lambda *x: print("Server listening on port 80")

app = Flask(__name__)

TEXT = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum."

def kill_later():
    def kill_thread():
        time.sleep(1)
        kill(getpid(), SIGTERM)
    threading.Thread(target=kill_thread).start()


@app.route("/", methods=["GET"])
def get():
    print("GET: Request completed")
    return "GET"


@app.route("/", methods=["POST"])
def post():
    print("POST: Request completed")
    return "POST"


@app.route("/", methods=["PUT"])
def put():
    print("PUT: Request completed")
    return "PUT"


@app.route("/", methods=["DELETE"])
def delete():
    print("DELETE: Request completed")
    kill_later()
    return "DELETE"


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=80)
