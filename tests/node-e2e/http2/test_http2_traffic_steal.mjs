import http2 from 'node:http2';

const server = http2.createServer();

server.on('error', (err) => {
  console.error(err);
  process.exit(-1);
});

server.on('stream', (stream, headers) => {
  console.log(`> Request ${JSON.stringify(headers)}`);

  // stream is a Duplex
  stream.respond({
    'content-type': 'text/html; charset=utf-8',
    ':status': 200,
  });

  stream.end('<h1>Hello World: <b>from local app</b></h1>');

  stream.on('error', (fail) => {
    console.error(`> Stream failed with ${fail}!`);
    process.exit(-2);
  });

  stream.on('aborted', () => {
    console.error(`> Stream was aborted!`);
    process.exit(-3);
  });

  stream.on('timeout', () => {
    console.error(`> Stream timed out!`);
    process.exit(-4);
  });

  stream.on('end', () => {
    console.log(`> Stream is done.`);
    stream.close();
  });

  stream.on('close', () => {
    console.log('> stream closed');
    server.close();
    process.kill(process.pid);
  })
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
