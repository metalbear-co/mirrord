const net = require("net")

const client = net.createConnection(
    80,
    'test-server',
    () => console.log("connection established"),
);

const expected = 20;
let received = 0;

client.on('data', (data) => {
    console.log(`received ${data.length} bytes of data`);
    received += data.length
});

client.on('close', (hadError) => {
    console.log(`connection closed with ${hadError ? 'error' : 'no error'}, received ${received} bytes, expected ${expected} bytes`);

    if (hadError || received !== expected) {
        process.exit(-1);
    } else {
        process.exit(0);
    }
});

client.on('error', (error) => {
    console.log(`connection error: ${error.message}`);
});
