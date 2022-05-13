var net = require("net");
var server = net.createServer();
var fs = require("fs");

fs.open("/var/log/dpkg.log", "r", (err, data) => {
  if (err) {
    console.error("Error opening file: ", err);
  }
});

//   console.log("Opened file: ", data);
// });

// fs.readFile("/home/alexc/dev/mirrord/Cargo.toml", "utf8", (err, data) => {
//   if (err) {
//     console.error("Error loading file: ", err);
//   }

//   console.log("Loaded file: ", data);

// });

server.on("connection", handleConnection);
server.listen(
  {
    host: "localhost",
    port: 80,
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
