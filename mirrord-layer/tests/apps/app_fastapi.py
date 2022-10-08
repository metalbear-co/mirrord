import time
import threading
from fastapi import FastAPI, Response
from os import getpid, kill
from signal import SIGTERM

app = FastAPI()
done = [False] * 4

def kill_later():
    def kill_thread():
        time.sleep(1)
        kill(getpid(), SIGTERM)
    threading.Thread(target=kill_thread).start()

@app.get("/")
def get():
    print("GET: Request completed")
    done[0] = True
    if done[1] and done[2] and done[3]:
        kill_later()
    return Response(content = "GET", media_type="text/plain")

@app.post("/")
def post():
    print("POST: Request completed")
    done[1] = True
    if done[0] and done[2] and done[3]:
        kill_later()
    return Response(content = "POST", media_type="text/plain")

@app.put("/")
def put():
    print("PUT: Request completed")
    done[2] = True
    if done[0] and done[1] and done[3]:
        kill_later()
    return Response(content = "PUT", media_type="text/plain")

@app.delete("/")
def delete():
    print("DELETE: Request completed")
    done[3] = True
    if done[0] and done[1] and done[2]:
        kill_later()
    return Response(content = "DELETE", media_type="text/plain")
