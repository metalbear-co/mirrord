import http from 'node:http';

const server = http.createServer();

// Listen to the request event
server.on('request', (request, response) => {
  console.log(`${request.method}: Request completed`);

  response.writeHead(200, {
    'content-type': 'text/html; charset=utf-8',
  });
  response.end(request.method);

  response.on('finish', () => {
    if (request.method == 'DELETE') {
      server.close();
    }
  });
});

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

server.listen(80);
