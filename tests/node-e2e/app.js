const express = require("express");
const crypto = require("crypto");
const process = require("process");
const fs = require("fs");
const app = express();
const PORT = 80;

const TEXT =
  "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";
var path = process.cwd() + crypto.randomBytes(16).toString("hex");

app.get("/", (req, res) => {
  console.log("GET: Request completed");
  res.send("GET"); // Todo: validate responses
});

app.post("/", (req, res) => {
  req.on("data", (data) => {
    if (data.toString() == TEXT) {
      console.log("POST: Request completed");
      req.send("POST");
    }
  });
});

app.put("/", (req, res) => {
  req.on("data", (data) => {
    fs.writeFile(path, data.toString(), (err) => {
      if (err) {
        throw err;
      }
      req.send("DELETE");
    });    
  });
  console.log("PUT: Request completed");
});

app.delete("/", (req, res) => {
  req.on("data", (data) => {
    fs.unlink(path, (err) => {
      if (err) {
        console.err("app.js failed with ", err);
        throw err;
      }
      req.send("DELETE");
    });
  });
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