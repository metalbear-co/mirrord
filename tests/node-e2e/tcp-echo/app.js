var net = require('net');
var server = net.createServer();
server.on('connection', handleConnection);
server.listen(80, function () {
  console.log('server listening to %j', server.address());
});
function handleConnection(conn) {
  var remoteAddress = conn.remoteAddress + ':' + conn.remotePort;
  console.log('new client connection from %s', remoteAddress);
  conn.on('data', onConnData);
  conn.once('close', onConnClose);
  conn.on('error', onConnError);

  function onConnData(d) {
    console.log("LOCAL APP GOT DATA:");
    console.log(d.toString());
    conn.write("local: ".concat(d));
  }
  function onConnClose() {
    console.log('connection from %s closed', remoteAddress);
  }
  function onConnError(err) {
    console.log('Connection %s error: %s', remoteAddress, err.message);
  }
}

