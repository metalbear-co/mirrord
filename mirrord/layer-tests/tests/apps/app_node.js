const express = require("express");
const process = require("process");
const app = express();
const PORT = 80;

var done = {
  "GET": false,
  "POST": false,
  "PUT": false,
  "DELETE": false,
}


function exit() {
  server.close();
  process.exit();
}

function get_handler(method) {
  return function (req, res) {
    console.log(method + ": Request completed");
    done[method] = true;
    if (Object.values(done).every(Boolean))
      exit();
    // The intproxy only lingers briefly on a mirror connection, so by the time
    // we respond the local connection may already be torn down. The response is
    // discarded anyway (nothing reads it back), so swallow any write error and
    // rely on `done` having been set above to drive the exit.
    try {
      res.send(method);
    } catch (_err) {}
  }
}

app.get("/", get_handler("GET"));
app.post("/", get_handler("POST"));
app.put("/", get_handler("PUT"));
app.delete("/", get_handler("DELETE"));


var server = app.listen(PORT, () => {
  console.log(`Server listening on port ${PORT}`);
});

// To exit gracefully
process.on('SIGTERM', () => {
  console.log("shutdown");
  server.close();
});