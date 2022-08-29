import https from "node:https";
import { createServer } from "net";

console.log(">> test_outgoing_traffic_make_request_after_listen");

const makeRequest = () => {
  const options = {
    hostname: "www.rust-lang.org",
    port: 443,
    path: "/",
    method: "GET",
  };

  const request = https.request(options, (response) => {
    console.log(`statusCode: ${response.statusCode}`);

    response.on("data", (data) => {
      process.stdout.write(data);
    });

    response.on("error", (fail) => {
      console.error(`>> response failed with ${fail}`);

      process.exit(-1);
    });

    if (response.statusCode === 200) {
      process.exit();
    } else {
      process.exit(-1);
    }
  });

  request.on("error", (fail) => {
    console.error(`>> request failed with ${fail}`);

    process.exit(-1);
  });

  request.end();
};

const listen = () => {
  const server = createServer();
  server.listen(
    {
      host: "localhost",
      port: 80,
    },
    function () {
      console.log(">> server listening to %j", server.address());

      makeRequest();
    }
  );

  server.on("error", (fail) => {
    console.error(">> createServer failed with `${fail}`");
    process.exit(-1);
  });

  server.on("connection", (socket) => {
    const remoteAddress = socket.remoteAddress + ":" + socket.remotePort;
    console.log(">> new client connection from %s", remoteAddress);

    makeRequest();

    conn.on("data", (data) => {
      console.log(
        ">> connection data from %s: %j",
        remoteAddress,
        data.toString()
      );
      conn.write(d);
    });

    conn.once("close", () => {
      console.log(">> connection from %s closed", remoteAddress);
    });

    conn.on("error", (fail) => {
      console.error(">> Connection %s error: %s", remoteAddress, err.message);
      process.exit(-1);
    });
  });
};

listen();
