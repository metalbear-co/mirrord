import dgram from 'node:dgram';
import { argv } from 'node:process';

// Defaults
let server_address = '127.0.0.1'
let server_port = '31415';

// Arguments override defaults if present.
// First argument is port, second is address (so that you can override only port).
if (argv.length >= 3) {
    server_port = argv[2];
    if (argv.length >= 4) {
        server_address = argv[3];
    }
}

console.log(`trying to talk over UDP with ${server_address}:${server_port}`)

const client_socket = dgram.createSocket('udp4');
client_socket.connect(server_port, server_address);

client_socket.on('error', (err) => {
    console.error(err);
    process.exit(-1);
});

client_socket.on('connect', () => {
    client_socket.send('Can I pass the test please?\n', (err) => {
        if (err) {
            console.log(err);
            process.exit(-1);
        }

        console.log('message sent, closing socket');
        client_socket.close();
    });
});

client_socket.on('close', () => {
    console.log('socket closed, exiting');
    process.exit(0);
});
