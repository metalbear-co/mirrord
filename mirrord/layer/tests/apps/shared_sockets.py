import threading
import time
from enum import Enum, unique
from os import getpid, kill
from signal import SIGTERM

from fastapi import FastAPI, Response

app = FastAPI()

@unique
class HttpMethod(str, Enum):
    GET = "GET"

def kill_later():
    def kill_thread():
        time.sleep(1)
        kill(getpid(), SIGTERM)

    threading.Thread(target=kill_thread).start()


def handle_request(method: HttpMethod):
    print(f'{method}: Request completed')
    kill_later()
    return Response(content=method, media_type="text/plain")

@app.get("/")
def get():
    return handle_request(HttpMethod.GET)
