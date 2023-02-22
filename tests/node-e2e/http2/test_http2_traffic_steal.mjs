import http2 from 'node:http2';

const server = http2.createServer();

// Listen to the request event
server.on('request', (request, response) => {
  console.log(`> Request ${JSON.stringify(request.headers)}`);

  response.writeHead(200, {
    'content-type': 'text/html; charset=utf-8',
  });

  response.end('<h1>Hello HTTP/2: <b>from local app</b></h1>');

  response.on('finish', () => {
    console.log(`> response is done ${JSON.stringify(response.getHeaders())}`);
    
    if (request.method == 'DELETE') {
      console.log('> DELETE request, closing the app');

      server.close();
      process.exit();
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
