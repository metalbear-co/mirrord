import { createServer } from "net";

console.log(">> test_listen_localhost");

const listen = () => {
  const server = createServer();
  server.listen(
    {
      host: "localhost",
      port: 80,
    },
    function () {
      console.log(">> server listening to %j", server.address());

      process.exit();
    }
  );

  server.on("error", (fail) => {
    console.error(`>> createServer failed with ${fail}`);
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
      console.error(
        `>> failed connection to ${remoteAddress} with ${err.message}`
      );

      throw fail;
    });
  });
};

listen();
