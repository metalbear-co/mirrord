import https from "node:https";

console.log(">> test_outgoing_traffic_many_requests");

const hostList = [
  "www.rust-lang.org",
  "www.github.com",
  "www.google.com",
  "www.bing.com",
  "www.yahoo.com",
  "www.twitter.com",
  "www.microsoft.com",
  "www.youtube.com",
  "www.live.com",
  "www.msn.com",
  "www.google.com.br",
  "www.yahoo.co.jp"
];

let requestIndex = 0;

function makeRequests() {
  hostList.forEach((host) => {
    const options = {
      hostname: host,
      port: 443,
      path: "/",
      method: "GET",
    };

    console.log(`>> host ${host}`);

    const request = https.request(options, (response) => {
      requestIndex += 1;
      console.log(
        `>> ${requestIndex} ${host} statusCode ${response.statusCode}`
      );

      response.on("data", (data) => {
        process.stdout.write(`>> received ${data.slice(0, 4)}`);
      });

      response.on("error", (fail) => {
        process.stderr.write(`>> response from ${host} failed with ${fail}`);
        throw fail;
      });
    });

    request.on("error", (fail) => {
      process.stderr.write(
        `>> request to ${requestIndex} ${host} failed with ${fail}`
      );
      throw fail;
    });

    request.end();
  });
}

for (let i = 0; i < 1; i++) {
  makeRequests();
}
