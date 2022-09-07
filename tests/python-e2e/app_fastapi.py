from typing import Union

from fastapi import FastAPI

app = FastAPI()

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
    return "DELETE"
