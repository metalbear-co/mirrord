from os import getcwd, remove
import click
from flask import Flask, request
import logging
import sys

log = logging.getLogger('werkzeug')
log.disabled = True

cli = sys.modules['flask.cli']

cli.show_server_banner = lambda *x: click.echo("Server listening on port 80")

app = Flask(__name__)

TEXT = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum."
PATH = getcwd() + "/test"


@app.route('/', methods=['GET'])
def get():
    print("GET: Request completed", flush=True)
    return "OK"


@app.route('/', methods=['POST'])
def post():
    if str(request.data, 'utf-8') == TEXT:
        print("POST: Request completed", flush=True)
    return "OK"


@app.route('/', methods=['PUT'])
def put():
    with open(PATH, "w") as f:
        f.write(TEXT)
    print("PUT: Request completed", flush=True)
    return "OK"


@app.route('/', methods=['DELETE'])
def delete():
    remove(PATH)
    print("DELETE: Request completed", flush=True)
    return "OK"


if __name__ == '__main__':
    app.run(host="0.0.0.0", port=80)
