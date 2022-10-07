const express = require("express");
const process = require("process");
const app = express();
const PORT = 80;

app.get("/", (req, res) => {
  console.log("GET: Request completed");
  res.send("GET");
});

app.post("/", (req, res) => {
  console.log("POST: Request completed");
  res.send("POST");
});

app.put("/", (req, res) => {
  console.log("PUT: Request completed");
  res.send("PUT");
});

app.delete("/", (req, res) => {
  console.log("DELETE: Request completed");
  res.send("DELETE");
  server.close();
  process.kill(process.pid)
});

var server = app.listen(PORT, () => {
  console.log(`Server listening on port ${PORT}`);
});

// To exit gracefull
process.on('SIGTERM', () => {
  console.log("shutdown");
  server.close();
});