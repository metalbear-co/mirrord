import { createServer, Socket } from "net";

const HOST = 'localhost';
const PORT = 80;
const MESSAGE = 'hello there';

const makeConnectionToSelf = () => {
  console.log(`making a TCP connection to ${HOST}:${PORT}`);

  const socket = new Socket();

  socket.on("error", (error) => {
    console.error(`an error occurred in the client: ${error}`);
    process.exit(1);
  });

  socket.on("connect", () => {
    const peerAddr = `${socket.remoteAddress}:${socket.remotePort}`;
    console.log(`made a connection to ${peerAddr}`);

    let receivedData = '';
    socket.on("data", (data) => {
      console.log(`received data from ${peerAddr}: ${data.toString()}`);
      receivedData += data.toString();

      if (receivedData === MESSAGE) {
        console.log(`received all data, closing closing the connection with ${peerAddr}`);
        socket.destroy();
      }
    });

    socket.write(`hello there`);

  });

  socket.connect(PORT, HOST);
};

const listen = () => {
  console.log(`starting a TCP server on ${HOST}:${PORT}`);

  const server = createServer();

  server.on("error", (error) => {
    console.error(`an error occurred in the server: ${error}`);
    process.exit(1);
  });

  server.on("connection", (socket) => {
    const peerAddr = socket.remoteAddress + ":" + socket.remotePort;

    console.log(`accepted a connection from ${peerAddr}`);

    socket.on("data", (data) => {
      console.log(`received data from ${peerAddr}: ${data.toString()}`);
      socket.write(data);
    });

    socket.on("close", () => {
      console.log(`connection from ${peerAddr} closed`);
      process.exit(0);
    });

    socket.on("error", (error) => {
      console.error(`connection from ${peerAddr} failed: ${error}`);
      process.exit(1);
    });
  });

  server.listen(
    {
      host: HOST,
      port: PORT,
    },
    () => {
      console.log(`TCP server listening on ${HOST}:${PORT}`);
      makeConnectionToSelf();
    }
  );
};

listen();
