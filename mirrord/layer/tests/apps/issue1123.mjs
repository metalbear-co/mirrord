import { createServer } from 'net';

const server = createServer();
server.listen(
  {
    host: "localhost",
    port: 80,
  },
  function () {
    console.log("First bind success!");
    
    const failServer = createServer();
    failServer.listen(
      {
        host: "localhost",
        port: 80,
      },
      function () {
        console.error("Second bind success, but it should fail!");
        exit(-1);
      });

    server.on("error", (fail) => {
      console.error(`First bind failed ${fail}`);
      throw fail;
    });

    failServer.on("error", (_) => {
      console.log(`Second bind failed successfully!`);
      process.exit();
    });

    failServer.on('listening', () => {
      console.error(`Second bind success, but it should fail!`);
      exit(-1);
    });
  }
);
