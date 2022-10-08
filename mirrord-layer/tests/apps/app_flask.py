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


def kill_later():
    def kill_thread():
        time.sleep(1)
        kill(getpid(), SIGTERM)
    threading.Thread(target=kill_thread).start()

done = [False] * 4

@app.route("/", methods=["GET"])
def get():
    print("GET: Request completed")
    done[0] = True
    if done[1] and done[2] and done[3]:
        kill_later()
    return "GET"


@app.route("/", methods=["POST"])
def post():
    print("POST: Request completed")
    done[1] = True
    if done[0] and done[2] and done[3]:
        kill_later()
    return "POST"


@app.route("/", methods=["PUT"])
def put():
    print("PUT: Request completed")
    done[2] = True
    if done[0] and done[1] and done[3]:
        kill_later()
    return "PUT"


@app.route("/", methods=["DELETE"])
def delete():
    print("DELETE: Request completed")
    done[3] = True
    if done[0] and done[1] and done[2]:
        kill_later()
    return "DELETE"


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=80)
