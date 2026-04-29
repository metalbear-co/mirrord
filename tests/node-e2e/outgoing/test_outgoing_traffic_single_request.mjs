import https from "node:https";

console.log(">> test_outgoing_traffic_single_request");

const options = {
  hostname: "rust-lang.org",
  port: 443,
  path: "/",
  method: "GET",
};

const request = https.request(options, (response) => {
  console.log(`>> statusCode: ${response.statusCode}`);

  response.on("data", (data) => {
    console.log(`>> response data ${data}`);
  });

  response.on("error", (fail) => {
    console.error(`>> response failed with ${fail}`);
    throw fail;
  });

  if (response.statusCode !== 200) {
    throw ">> response.statusCode !== 200";
  }
});

request.on("error", (fail) => {
  console.error(`>> request failed with ${fail}`);
  throw fail;
});

request.end();
