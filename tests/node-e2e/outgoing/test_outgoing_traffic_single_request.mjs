import https from "node:https";

console.log(">> test_outgoing_traffic_single_request");

const options = {
  hostname: "www.rust-lang.org",
  port: 443,
  path: "/",
  method: "GET",
};

const request = https.request(options, (response) => {
  console.log(`statusCode: ${response.statusCode}`);

  response.on("data", (data) => {
    process.stdout.write(data);
  });

  response.on("error", (fail) => {
    process.stderr.write(`>> response failed with ${fail}`);
    throw fail;
  });

  if (response.statusCode !== 200) {
    throw ">> response.statusCode !== 200";
  }
});

request.on("error", (fail) => {
  process.stderr.write(`>> request failed with ${fail}`);
  throw fail;
});

request.end();
