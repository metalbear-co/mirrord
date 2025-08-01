from fastapi import FastAPI, Response
from threading import Lock

print_lock = Lock()
app = FastAPI()

@app.get("/")
def get():
    with print_lock:
        print("GET: Request completed")
    return Response(content = "GET", media_type="text/plain")

@app.post("/")
def post():
    with print_lock:
        print("POST: Request completed")
    return Response(content = "POST", media_type="text/plain")

@app.put("/")
def put():
    with print_lock:
        print("PUT: Request completed")
    return Response(content = "PUT", media_type="text/plain")

@app.delete("/")
def delete():
    with print_lock:
        print("DELETE: Request completed")
    return Response(content = "DELETE", media_type="text/plain")
