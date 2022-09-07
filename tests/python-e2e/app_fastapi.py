import time
import threading
from fastapi import FastAPI
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
    return "GET"

@app.post("/")
def post():
    print("POST: Request completed")
    return "POST"

@app.put("/")
def put():
    print("PUT: Request completed")
    return "PUT"

@app.delete("/")
def delete():
    print("DELETE: Request completed")
    kill_later()
    return "DELETE"
