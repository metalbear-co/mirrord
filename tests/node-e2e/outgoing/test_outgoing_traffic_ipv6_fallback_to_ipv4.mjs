import https from "node:https";

console.log(">> test_outgoing_traffic_ipv6_fallback_to_ipv4");

// Tries the request over IPv6 first, then falls back to IPv4 - like apps with
// their own happy-eyeballs logic. With IPv6 enabled by default, the first
// attempt may open a real IPv6 socket and still fail on a cluster without an
// IPv6 route; the test passes only if the IPv4 fallback completes.
// Any HTTP response proves the connection worked - the status code depends on
// the remote server and is not what this test is about.
const baseOptions = {
  hostname: "www.rust-lang.org",
  port: 443,
  path: "/",
  method: "GET",
};

function makeRequest(family, onError) {
  const request = https.request({ ...baseOptions, family }, (response) => {
    console.log(
      `>> request succeeded over IPv${family} (statusCode: ${response.statusCode})`
    );
    process.exit(0);
  });

  request.on("error", (fail) => {
    console.log(`>> family ${family} request failed with ${fail}`);
    onError(fail);
  });

  request.end();
}

makeRequest(6, () => {
  console.log(">> falling back to IPv4");
  makeRequest(4, (fail) => {
    throw fail;
  });
});
