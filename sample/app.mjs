import { Buffer } from "node:buffer";
import { createServer } from "net";
import { open, readFile } from "fs/promises";
// var server = createServer();

async function debug_open() {
  try {
    const fileHandle = await open("/var/log/dpkg.log", "r");
    console.log(">>> open file ", fileHandle);

    fileHandle.close();
  } catch (fail) {
    console.error("!!! Failed opening file with ", fail);
  }
}

// debug_open();

async function debug_read() {
  try {
    const buffer = await readFile("/var/log/dpkg.log");
    console.log(">>> read file ", buffer);
  } catch (fail) {
    console.error("!!! Failed reading file with ", fail);
  }
}

// debug_read();

async function debug_fseek_read() {
  try {
    const fileHandle = await open("/var/log/dpkg.log", "r");
    console.log(">>> open file ", fileHandle);

    let buffer = Buffer.alloc(32);
    let buffer_result = await fileHandle.read(buffer, 0, 10, 32);

    console.log(">>> read fseek buffer ", buffer);
    console.log(">>> read fseek buffer_result ", buffer_result);

    fileHandle.close();
  } catch (fail) {
    console.error("!!! Failed using fseek with ", fail);
  }
}

// debug_fseek_read();

async function debug_fseek_readv() {
  try {
    const fileHandle = await open("/var/log/dpkg.log", "r");
    console.log(">>> open file ", fileHandle);

    let buffer = Buffer.alloc(32);
    let buffer_result = await fileHandle.read(buffer, 0, 10, 32);

    console.log(">>> read fseek buffer ", buffer);
    console.log(">>> read fseek buffer_result ", buffer_result);

    fileHandle.close();
  } catch (fail) {
    console.error("!!! Failed using fseek with ", fail);
  }
}

debug_fseek_read();

server.on("connection", handleConnection);
server.listen(
  {
    host: "localhost",
    port: 8000,
  },
  function () {
    console.log("server listening to %j", server.address());
  }
);

function handleConnection(conn) {
  var remoteAddress = conn.remoteAddress + ":" + conn.remotePort;
  console.log("new client connection from %s", remoteAddress);
  conn.on("data", onConnData);
  conn.once("close", onConnClose);
  conn.on("error", onConnError);

  function onConnData(d) {
    console.log("connection data from %s: %j", remoteAddress, d.toString());
    conn.write(d);
  }
  function onConnClose() {
    console.log("connection from %s closed", remoteAddress);
  }
  function onConnError(err) {
    console.log("Connection %s error: %s", remoteAddress, err.message);
  }
}

// TODO(aviram) [mid] 2022-05-20:
/*
process A loads our layer, our layer connects to k8s, creates agent and hooks.
process A then forks to process B, to do some offloading work (like read of Node.js)
process B isn't hooked or loaded (which is weird? as LD PRELOAD should pass down? we need to check that)
if we get it to pass down, we shouldn't load normally, we should load in a "fork" mode where we connect to process A
and use it for communicating with agent, as it has the "full state" (and we won't create an agent per fork)
once layerd loads for first time, it changes a flag env var to specify it was loaded, and forks should get the env var passed.
LAYERD_PARENT_FD = 10
each child communicates via LAYERD_PARENT_FD


*/
