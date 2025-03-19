import http from 'node:http';

const server = http.createServer();

server.on('error', (fail) => {
  console.error(`> server failed with ${fail}!`);
  process.exit(-2);
});

server.on('aborted', () => {
  console.error(`> server was aborted!`);
  process.exit(-3);
});

server.on('timeout', () => {
  console.error(`> server timed out!`);
  process.exit(-4);
});

server.on('listening', () => {
  console.log("SUCCESS");
  process.exit(0);
});

server.listen(80);
