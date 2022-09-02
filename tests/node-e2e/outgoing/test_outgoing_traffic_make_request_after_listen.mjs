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
      process.stderr.write(`>> response failed with ${fail}`);
      throw fail;
    });

    if (response.statusCode >= 400 && response.statusCode < 500) {
      throw `>> Failed with error status code ${response.statusCode}`;
    }
  });

  request.on("error", (fail) => {
    process.stderr.write(`>> request failed with ${fail}`);
    throw fail;
  });

  request.on("finish", () => {
    process.stdout.write(">> success");
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

      server.close();
    }
  );

  server.on("error", (fail) => {
    process.stderr.write(`>> createServer failed with ${fail}`);
    throw fail;
  });

  server.on("connection", (socket) => {
    const remoteAddress = socket.remoteAddress + ":" + socket.remotePort;
    console.log(">> new client connection from %s", remoteAddress);

    socket.on("data", (data) => {
      console.log(
        `>> connection data from ${remoteAddress}: %j`,
        data.toString()
      );

      socket.write(d);
    });

    socket.on("close", () => {
      console.log(">> connection from %s closed", remoteAddress);
    });

    socket.on("error", (fail) => {
      process.stderr.write(
        `>> failed connectio to ${remoteAddress} with ${err.message}`
      );

      throw fail;
    });
  });
};

listen();
