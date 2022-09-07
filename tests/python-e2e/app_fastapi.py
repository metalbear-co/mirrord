import time
import threading
from fastapi import FastAPI, Response
from os import getpid, kill
from signal import SIGTERM

app = FastAPI()

def kill_later():
    def kill_thread():
        time.sleep(1)
        kill(getpid(), SIGTERM)
    threading.Thread(target=kill_thread).start()

@app.get("/")
def get():
    print("GET: Request completed")
    return Response(content = "GET", media_type="text/plain")

@app.post("/")
def post():
    print("POST: Request completed")
    return Response(content = "POST", media_type="text/plain")

@app.put("/")
def put():
    print("PUT: Request completed")
    return Response(content = "PUT", media_type="text/plain")

@app.delete("/")
def delete():
    print("DELETE: Request completed")
    kill_later()
    return Response(content = "DELETE", media_type="text/plain")
