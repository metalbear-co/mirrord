import logging
import sys
import threading
import time
from enum import Enum, unique
from os import getpid, kill
from signal import SIGTERM

from flask import Flask
from flask import request

log = logging.getLogger("werkzeug")
log.disabled = True

cli = sys.modules["flask.cli"]

cli.show_server_banner = lambda *x: print("Server listening on port 80")

app = Flask(__name__)


@unique
class HttpMethod(Enum):
    GET = "GET"
    POST = "POST"
    PUT = "PUT"
    DELETE = "DELETE"


done = {method: False for method in HttpMethod}


def kill_later():
    def kill_thread():
        time.sleep(1)
        kill(getpid(), SIGTERM)

    threading.Thread(target=kill_thread).start()


def handle_request(method: HttpMethod):
    print(f'{method}: Request completed')
    done[method] = True
    if all(done.values()):
        kill_later()
    return str(method)


@app.route("/", methods=["GET"])
def get():
    server = request.server
    print(f'{server}: Server value friends')
    assert server == ("1.1.1.1", 80)
    return handle_request(HttpMethod.GET)


@app.route("/", methods=["POST"])
def post():
    return handle_request(HttpMethod.POST)


@app.route("/", methods=["PUT"])
def put():
    return handle_request(HttpMethod.PUT)


@app.route("/", methods=["DELETE"])
def delete():
    return handle_request(HttpMethod.DELETE)


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=80)
