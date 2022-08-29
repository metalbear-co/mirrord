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
      console.error(`>> response from ${host} failed with ${fail}`);

      process.exit(-1);
    });

    if (response.statusCode !== 200) {
      process.exit(-1);
    }
  });

  request.on("error", (fail) => {
    console.error(`>> request to ${host} failed with ${fail}`);

    process.exit(-1);
  });

  request.end();
}
