import { createServer } from 'net';

console.log("Testing that double binding on same address fails!");
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
        console.log("Second bind???");
        console.error("Second bind success, but it should fail!");
        exit(-1);
      });

    server.on("error", (fail) => {
      console.log("Second bind???");
      console.error(`First bind failed ${fail}`);
      throw fail;
    });

    failServer.on("error", (_) => {
      console.log(`Second bind failed successfully!`);
      process.exit();
    });
  }
);
