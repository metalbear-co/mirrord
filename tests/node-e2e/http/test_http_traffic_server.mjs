import http from "node:http";

const server = http.createServer((req, res) => {
    res.writeHead(200);
    res.write("Response from process!");

    console.log(`>> response is ${res}`);
    res.end();

    res.on("finish", () => {
        console.log(`>> response is done`)
        server.close();
    });

});

server.on("clientError", (err, socket) => {
    socket.end("HTTP/1.1 400 Bad Request\r\n\r\n");
});

server.listen(80);