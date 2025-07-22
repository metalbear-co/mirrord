import { createServer } from "net";

console.log(">> test_remote_remote_dns_enabled_works");

const server = createServer();
server.on("connection", (connection) => {});

server.listen(
  {
    host: "localhost",
    port: 80,
  },
  function () {
    console.log(">> server is listening ", server.address());

    process.exit(0);
  }
);
