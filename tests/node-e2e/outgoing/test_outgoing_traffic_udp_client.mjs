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

client_socket.send('Can I pass the test please?\n', server_port, server_address, (err) => {
    client_socket.close();
    if (err){
        throw err
    }
});
