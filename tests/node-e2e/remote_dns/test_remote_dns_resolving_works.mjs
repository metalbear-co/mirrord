import { createServer } from "net";

console.log(">> test_remote_remote_dns_resolving_works");

const server = createServer();
server.on("connection", handleConnection);
server.listen(
  {
    host: "localhost",
    port: 80,
  },
  function () {
    console.log("server listening to %j", server.address());

    process.exit(0);
  }
);

function handleConnection(conn) {
  var remoteAddress = conn.remoteAddress + ":" + conn.remotePort;
  console.log("new client connection from %s", remoteAddress);

  conn.on("error", onConnError);
  function onConnError(err) {
    throw err;
  }
}
