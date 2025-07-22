import https from "node:https";

console.log(">> test_outgoing_traffic_single_request_ipv6");

const options = {
  hostname: "www.rust-lang.org",
  port: 443,
  path: "/",
  method: "GET",
  family: 6,
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
    console.warn(
      ">> this test was designed to be run in a pod without ipv6 support!"
    );
    throw ">> response.statusCode !== 200";
  }
});

request.on("error", (fail) => {
  console.error(`>> request failed with ${fail}`);
  throw fail;
});

request.end();
