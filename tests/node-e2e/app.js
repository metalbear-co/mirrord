const express = require("express");
const process = require("process");
const app = express();
const PORT = 80;

const TEXT =
  "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";

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
});

var server = app.listen(PORT, () => {
  console.log(`Server listening on port ${PORT}`);
});

// To exit gracefull
process.on('SIGTERM', () => {
  console.log("shutdown");
  server.close();
});