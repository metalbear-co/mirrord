from enum import Enum, unique

from fastapi import BackgroundTasks, FastAPI, Response

app = FastAPI()

@unique
class HttpMethod(str, Enum):
    GET = "GET"

def handle_request(method: HttpMethod):
    background = BackgroundTasks()
    # Let the test stop the reload parent after the response send path returns.
    background.add_task(print, f'{method}: Request completed', flush=True)
    return Response(content=method, media_type="text/plain", background=background)

@app.get("/")
def get():
    return handle_request(HttpMethod.GET)
