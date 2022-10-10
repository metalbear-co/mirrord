const express = require("express");
const process = require("process");
const app = express();
const PORT = 80;
var done = new Array(4).fill(false);


function exit() {
  server.close();
  process.exit();
}

app.get("/", (req, res) => {
  console.log("GET: Request completed");
  res.send("GET");
  done[0] = true;
  if (done[1] && done[2] && done[3])
    exit();
});

app.post("/", (req, res) => {
  console.log("POST: Request completed");
  res.send("POST");
  done[1] = true;
  if (done[0] && done[2] && done[3])
    exit();
});

app.put("/", (req, res) => {
  console.log("PUT: Request completed");
  res.send("PUT");
  done[2] = true;
  if (done[0] && done[1] && done[3])
    exit();
});

app.delete("/", (req, res) => {
  console.log("DELETE: Request completed");
  res.send("DELETE");
  done[3] = true;
  if (done[0] && done[1] && done[2])
    exit();
});

var server = app.listen(PORT, () => {
  console.log(`Server listening on port ${PORT}`);
});

// To exit gracefully
process.on('SIGTERM', () => {
  console.log("shutdown");
  server.close();
});