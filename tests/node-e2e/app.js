const express = require("express");
const process = require("process");
const { spawn } = require('node:child_process');
const app = express();
const PORT = 80;

process.env.TEST = 'foobar';

const python = spawn('python3', ['-u', './tests/python-e2e/app_flusk.py']);

python.stdout.on('data', (data) => {
  console.log(`stdout: ${data}`);
});

python.stderr.on('data', (data) => {
  console.error(`stderr: ${data}`);
});

python.on('close', (code) => {
  console.log(`child process exited with code ${code}`);
});


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