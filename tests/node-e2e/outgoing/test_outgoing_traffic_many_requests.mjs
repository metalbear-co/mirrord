import https from "node:https";

const HOSTS = process.env.AVAILABLE_HOSTS;

function makeRequest(host) {
  const options = {
    hostname: host,
    port: 443,
    path: "/",
    method: "GET",
  };

  console.log(`making request to ${host}`);

  const request = https.request(options, (response) => {
    console.log(
      `got a response from ${host}: statusCode=${response.statusCode}`
    );

    response.on("data", (_data) => {});

    response.on("error", (error) => {
      console.error(`response from ${host} failed: ${error}`);
      process.exit(1);
    });
  });

  request.on("error", (error) => {
    console.error(`request to ${host} failed: ${error}`)
    process.exit(1);
  });

  request.end();
}

if (HOSTS === undefined) {
  console.error("`AVAILABLE_HOSTS` environment variable is missing");
  process.exit(1);
}

HOSTS.split(",").forEach(makeRequest);
