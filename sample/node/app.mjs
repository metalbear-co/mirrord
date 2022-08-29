import { Buffer } from "node:buffer";
import { createServer, Socket } from "net";
import { open, readFile } from "fs/promises";
import https from "node:https";
import http from "node:http";
import { exit } from "node:process";
import dns from "node:dns";

async function debug_file_ops() {
  try {
    const readOnlyFile = await open("/var/log/dpkg.log", "r");
    console.log(">>>>> open readOnlyFile ", readOnlyFile);

    let buffer = Buffer.alloc(128);
    let bufferResult = await readOnlyFile.read(buffer);
    console.log(">>>>> read readOnlyFile returned with ", bufferResult);

    const sampleFile = await open("/tmp/node_sample.txt", "w+");
    console.log(">>>>> open file ", sampleFile);

    const written = await sampleFile.write("mirrord sample node");
    console.log(">>>>> written ", written, " bytes to file ", sampleFile);

    let sampleBuffer = Buffer.alloc(32);
    let sampleBufferResult = await sampleFile.read(buffer);
    console.log(">>>>> read ", sampleBufferResult, " bytes from ", sampleFile);

    readOnlyFile.close();
    sampleFile.close();
  } catch (fail) {
    console.error("!!! Failed file operation with ", fail);
  }
}

function debugDns() {
  dns.lookup("www.google.com", null, (err, address, family) => {
    console.log("address: %j family: IPv%s", address, family);
  });
}

// debug_file_ops();
// debugDns();
debugRequest("www.google.com");
debugRequest("www.rust-lang.org");
// debugRequest("www.google.com");
// debugConnect();
debugListen();

function debugConnect() {
  let options = { readable: true, writable: true };
  let socket = new Socket(options);

  socket.connect({ port: 80, host: "20.81.111.66", localPort: 8888 });

  socket.on("connect", () => {
    console.log(">> connected!");
    exit(0);
  });

  socket.on("error", (fail) => {
    console.error(">> failed connect ", fail);
  });
}

function debugRequest(host) {
  const options = {
    // hostname: "20.81.111.66",
    // port: 80,
    hostname: host,
    // hostname: host,
    port: 443,
    path: "/",
    method: "GET",
  };

  console.log(">> options ", options);

  // const req = http.request(options, (res) => {
  const req = https.request(options, (res) => {
    console.log(`statusCode: ${res.statusCode}`);

    res.on("data", (d) => {
      console.log(">> data -> host ", host, " | bytes ", d);

      // process.stdout.write(d.slice(0, 32));
    });

    res.on("close", () => {
      console.log(">> response is done, closing!");
    });
  });

  req.on("socket", (socket) => {
    console.log(
      ">> socket local: addr ",
      socket.localAddress,
      ":",
      socket.localPort,
      " remote: ",
      socket.remoteAddress,
      ":",
      socket.remotePort
    );

    socket.on("lookup", (fail, address, family, host) => {
      console.log(
        ">> socket lookup -> fail ",
        fail,
        " address ",
        address,
        " family ",
        family,
        " host ",
        host
      );
    });

    socket.on("error", (fail) => {
      console.error(">> socket error -> fail ", fail);
    });
  });

  req.on("close", () => {
    console.log(">> close");
  });

  req.on("continue", () => {
    console.log(">> continue");
  });

  req.on("finish", () => {
    console.log(">> finish");
  });

  req.on("response", () => {
    console.log(">> response");
  });

  req.on("timeout", () => {
    console.log(">> timeout");
  });

  req.on("upgrade", () => {
    console.log(">> upgrade");
  });

  req.on("connect", (incoming, socket, head) => {
    console.log(">> connect ", incoming, " ", socket, " ", head);
  });

  req.on("information", (info) => {
    console.log(">> information ", info);
  });

  req.on("error", (error) => {
    console.error(error);
  });

  req.end();
}

function debugListen() {
  const server = createServer();
  server.on("connection", handleConnection);
  server.listen(
    {
      host: "localhost",
      port: 80,
    },
    function () {
      console.log(">> server listening to %j", server.address());

      debugRequest("www.bing.com");
    }
  );

  function handleConnection(conn) {
    var remoteAddress = conn.remoteAddress + ":" + conn.remotePort;
    console.log(">> new client connection from %s", remoteAddress);

    conn.on("data", onConnData);
    conn.once("close", onConnClose);
    conn.on("error", onConnError);

    function onConnData(d) {
      console.log(
        ">> connection data from %s: %j",
        remoteAddress,
        d.toString()
      );
      conn.write(d);
    }

    function onConnClose() {
      console.log(">> connection from %s closed", remoteAddress);
    }

    function onConnError(err) {
      console.error(">> Connection %s error: %s", remoteAddress, err.message);
    }
  }
}
