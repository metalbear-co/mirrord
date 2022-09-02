import { lookup } from "dns/promises";

console.log(">> test_outgoing_traffic_single_request");

const makeRequest = () => {
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

    if (response.statusCode >= 400 && response.statusCode < 500) {
      throw `>> Failed with error status code ${response.statusCode}`;
    }
  });

  request.on("error", (fail) => {
    process.stderr.write(`>> request failed with ${fail}`);
    throw fail;
  });

  request.on("finish", () => {
    process.stdout.write(">> success");
    process.exit();
  });

  request.end();
};
