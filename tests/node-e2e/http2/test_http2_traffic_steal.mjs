import http2 from 'node:http2';

const server = http2.createServer();

// Listen to the request event
server.on('request', (request, response) => {
  console.log(`> Request ${JSON.stringify(request.headers)}`);

  response.writeHead(200, {
    'content-type': 'text/html; charset=utf-8',
  });

  response.end('<h1>Hello World: <b>from local app</b></h1>');

  response.on('finish', () => {
    console.log(`> response is done.`);
    server.close();
  });

  request.on('close', () => {
    console.log(`> request is done.`);
    server.close();
  })
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

server.on('close', () => {
  console.log('> server close');
  process.exit();
});

server.listen(80);

process.on('SIGTERM', () => {
  console.log('> shutdown');
  server.close();
});
