const express = require("express");
const process = require("process");
const app = express();
const PORT = 80;

app.get("/", (req, res) => {
  console.log("GET: Request completed");
  res.send("Request received"); // Todo: validate responses
});

app.post("/", (req, res) => {
  console.log("POST: Request completed");
});

app.put("/", (req, res) => {
  console.log("PUT: Request completed");
});

app.delete("/", (req, res) => {
  console.log("DELETE: Request completed");
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