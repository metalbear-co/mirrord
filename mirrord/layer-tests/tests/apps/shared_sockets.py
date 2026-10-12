from enum import Enum, unique

from fastapi import BackgroundTasks, FastAPI, Response

app = FastAPI()

@unique
class HttpMethod(str, Enum):
    GET = "GET"

def handle_request(method: HttpMethod):
    background = BackgroundTasks()
    # The test sends SIGTERM after this line, so print it after sending the response.
    # stdout is a pipe; flush the line so Python buffering cannot delay shutdown.
    background.add_task(print, f'{method}: Request completed', flush=True)
    return Response(content=method, media_type="text/plain", background=background)

@app.get("/")
def get():
    return handle_request(HttpMethod.GET)
