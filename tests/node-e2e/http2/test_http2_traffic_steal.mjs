import http2 from 'node:http2';

const server = http2.createServer();

server.on('error', (err) => {
    console.error(err);
    process.exit(-1);
});

server.on('stream', (stream, headers) => {
    // stream is a Duplex
    stream.respond({
        'content-type': 'text/html; charset=utf-8',
        ':status': 200,
    });

    stream.end('<h1>Hello World: from local</h1>');

    stream.on('error', (fail) => {
        console.error(`Stream failed with ${fail}!`);
        process.exit(-2);
    });

    stream.on('aborted', () => {
        console.error(`Stream was aborted!`);
        process.exit(-3);
    });

    stream.on('timeout', () => {
        console.error(`Stream timed out!`);
        process.exit(-4);
    });

    stream.on('end', () => {
        console.log(`Stream is done.`);

        process.exit();
    });
});

server.listen(80);
