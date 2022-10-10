import https from "node:https";

console.log(">> test_outgoing_traffic_many_requests");

const hostList = [
  "www.rust-lang.org",
  "www.github.com",
  "www.google.com",
  "www.bing.com",
  "www.yahoo.com",
  "www.baidu.com",
  "www.twitter.com",
  "www.microsoft.com",
  "www.youtube.com",
  "www.live.com",
  "www.msn.com",
  "www.google.com.br",
  "www.yahoo.co.jp",
  "www.qq.com",
];

let requestIndex = 0;

function makeRequests() {
  let failures = 0;
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

      response.on("error", (fail) => {
        console.log(`>> response from ${host} failed with ${fail}`);
        if (fail.errno !== -104 || fail.code !== 'ECONNRESET') {
          console.log("This is not a tolerated error, fail test.")
          throw fail;
        }
        if (++failures > 1) {
          console.log("Too many failed requests, failing test.");
          throw fail;
        } else {
          console.log("Allowing one failed request, not to fail the CI on random resets.");
        }
      });
    });

    request.on("error", (fail) => {
      console.log(`>> request to ${requestIndex} ${host} failed with ${fail}`);
      if (fail.errno !== -104 || fail.code !== 'ECONNRESET') {
        console.log("This is not a tolerated error, fail test.")
        throw fail;
      }
      if (++failures > 1) {
        console.log("Too many failed requests, failing test.");
        throw fail;
      } else {
        console.log("Allowing one failed request, not to fail the CI on random resets.");
      }
    });

    request.end();
  });
}

for (let i = 0; i < 1; i++) {
  makeRequests();
}
