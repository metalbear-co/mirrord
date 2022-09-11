import https from "node:https";

console.log(">> test_outgoing_traffic_single_request");

const options = {
  hostname: "www.rust-lang.org",
  port: 443,
  path: "/",
  method: "GET",
  family: 6,
};

// TODO(alex) [high] 2022-09-08: Ideas for ipv6 fix:
//
//  1. Give precedence for ipv4, and error on ipv6-only requests;
//    - seems to be the most faithful to what could happen when the user properly deploys;
//
//  2. Try to connect with with ipv6, on fail, retry with ipv4;
//    - how do I get the ipv4 for the resolved address?
//    - would probably need to store all resolved addresses, and then check for one that is ipv4;
//    - would this be a behavior change? when the user deploys, if they're resolving ipv6 their app
//    will fail (as it works only due to mirrord changing this behavior);
//
//  3. Just error out if it doens't support ipv6;
//
//  4. Drop ipv6 queries (instead of error, just warn);
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
