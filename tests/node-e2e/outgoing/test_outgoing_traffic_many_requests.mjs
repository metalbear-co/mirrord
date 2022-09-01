import https from "node:https";

console.log(">> test_outgoing_traffic_many_requests");

const hostList = [
  "www.rust-lang.org",
  "www.nodejs.org",
  "www.google.com",
  "www.bing.com",
  "www.github.com",
];

hostList.forEach((host) => {
  const options = {
    hostname: host,
    port: 443,
    path: "/",
    method: "GET",
  };

  console.log(`>> host ${host}`);

  const request = https.request(options, (response) => {
    console.log(`>> ${host} statusCode ${response.statusCode}`);

    response.on("data", (data) => {
      process.stdout.write(`>> received ${data.slice(0, 16)}`);
    });

    response.on("error", (fail) => {
      process.stderr.write(`>> response from ${host} failed with ${fail}`);
      throw fail;
    });
  });

  request.on("error", (fail) => {
    process.stderr.write(`>> request to ${host} failed with ${fail}`);
    throw fail;
  });

  request.end();
});
