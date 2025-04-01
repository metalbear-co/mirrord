from fastapi import FastAPI, Response

app = FastAPI()

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
    return Response(content = "DELETE", media_type="text/plain")
