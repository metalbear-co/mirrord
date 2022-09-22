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

const client_socket = dgram.createSocket('udp4');

client_socket.on('error', (err) => {
        if (err) {
            throw err
        }
    }
);
console.log(`using port ${server_port} and host ${server_address}`);
client_socket.bind('31413'); // Currently mirrord will ignore binding to port 0.
client_socket.connect(server_port, server_address, (connect_err) => {
    if (connect_err) {
        throw connect_err
    }
    client_socket.send('Can I pass the test please?\n', (send_err) => {
        if (send_err) {
            throw send_err
        }
    });
    client_socket.close();
});

