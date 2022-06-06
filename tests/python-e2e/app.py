from os import _exit, getcwd
from flask import Flask, request
import logging

log = logging.getLogger('werkzeug')
log.disabled = True

app = Flask(__name__)

TEXT = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum."
PATH = getcwd() + "/test"

@app.route('/', methods=['GET'])
def get():    
    print("GET: request received")
    return "OK"

@app.route('/', methods=['POST'])
def post():    
    if str(request.data) == TEXT:
        print("POST: request received")
        return "OK"
    else:
        _exit(1)

@app.route('/', methods=['PUT'])        
def put():
    pass

@app.route('/', methods=['DELETE'])
def delete():
    pass

if __name__ == '__main__':
    app.run(host="0.0.0.0", port=80)