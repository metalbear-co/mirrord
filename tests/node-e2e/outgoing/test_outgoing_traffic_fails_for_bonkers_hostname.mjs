import https from "node:https";

console.log(">> test_outgoing_traffic_fails_for_bonkers_hostname");

const options = {
  hostname: "www.bonkers1234.com.net",
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
    console.error(`>> response failed with ${fail}`);

    process.exit(-1);
  });

  if (response.statusCode === 200) {
    process.exit();
  } else {
    process.exit(-1);
  }
});

request.on("error", (fail) => {
  console.error(`>> request failed with ${fail}`);

  process.exit(-1);
});

request.end();
