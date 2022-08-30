import https from "node:https";

console.log(">> test_outgoing_traffic_many_requests");

const hostList = [
  "www.rust-lang.org",
  "www.nodejs.org",
  "www.google.com",
  "www.bing.com",
  "www.github.com",
];

for (let host in hostList) {
  const options = {
    hostname: host,
    port: 443,
    path: "/",
    method: "GET",
  };

  const request = https.request(options, (response) => {
    console.log(`>> ${host} statusCode ${response.statusCode}`);

    response.on("data", (data) => {
      process.stdout.write(data);
    });

    response.on("error", (fail) => {
      process.stderr.write(`>> response from ${host} failed with ${fail}`);
      throw fail;
    });

    if (response.statusCode !== 200) {
      throw ">> response.statusCode !== 200";
    }
  });

  request.on("error", (fail) => {
    process.stderr.write(`>> request to ${host} failed with ${fail}`);
    throw fail;
  });

  request.end();
}
