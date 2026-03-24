/**
 * This application reads a comma-separated list of socket address from the `TO_HOSTS` environment variable.
 * 
 * Attempts to make a TCP connection to each of the addresses.
 * Should any connection attempt fail, it fails.
 * When all connections have succeeded, the app exits.
 */

import { Socket } from "net";

const TO_HOSTS = process.env.TO_HOSTS;

let successful_connections = 0;

function makeConnection(host) {
    const chunks = host.split(":");
    const port = parseInt(chunks[1]);
    const addr = chunks[0];

    const socket = new Socket()

    socket.on("error", (error) => {
        console.error(`an IO error occurred: ${error}`);
        process.exit(1);
    });

    socket.on("connect", () => {
        const peerAddr = `${socket.remoteAddress}:${socket.remotePort}`;
        console.log(`made a connection to ${host} (${peerAddr})`);
        successful_connections += 1;
        if (TO_HOSTS.split(",").length == successful_connections) {
            process.exit(0)
        }
    });

    socket.connect(port, addr);
}

if (TO_HOSTS === undefined) {
    console.error("`TO_HOSTS` environment variable is missing");
    process.exit(1);
}

const hosts = TO_HOSTS.split(",")
if (hosts.length === 0) {
    process.exit(0);
}
hosts.forEach(makeConnection);
