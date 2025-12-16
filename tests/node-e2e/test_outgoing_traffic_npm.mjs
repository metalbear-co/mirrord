import https from "node:https";

console.log(">> npm-based outgoing traffic test");

const options = {
  hostname: "rust-lang.org",
  port: 443,
  path: "/",
  method: "GET",
};

const request = https.request(options, (response) => {
  console.log(`>> statusCode: ${response.statusCode}`);

  response.on("data", (data) => {
    console.log(`>> received data chunk of size ${data.length}`);
  });

  response.on("error", (fail) => {
    console.error(`>> response failed with ${fail}`);
    throw fail;
  });

  if (response.statusCode !== 200) {
    throw new Error(`>> response.statusCode !== 200, got ${response.statusCode}`);
  }

  response.on("end", () => {
    console.log(">> npm outgoing test completed successfully");
  });
});

request.on("error", (fail) => {
  console.error(`>> request failed with ${fail}`);
  throw fail;
});

request.end();