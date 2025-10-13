import https from "node:https";

console.log(">> npm outgoing test starting");

// Test that npm was able to run and this script is executing
console.log(">> npm executed successfully");

// Test outgoing HTTP connection
const options = {
  hostname: "httpbin.org",
  port: 443,
  path: "/get",
  method: "GET",
};

console.log(">> making outgoing HTTPS request to httpbin.org");

const request = https.request(options, (response) => {
  console.log(`>> statusCode: ${response.statusCode}`);
  
  let data = '';
  response.on("data", (chunk) => {
    data += chunk;
    console.log(`>> received ${chunk.length} bytes`);
  });

  response.on("end", () => {
    console.log(`>> response complete, total bytes: ${data.length}`);
    
    try {
      const jsonData = JSON.parse(data);
      console.log(`>> parsed JSON response with origin: ${jsonData.origin}`);
      console.log(">> npm outgoing test completed successfully");
    } catch (e) {
      console.error(`>> failed to parse JSON: ${e}`);
      process.exit(1);
    }
  });

  response.on("error", (fail) => {
    console.error(`>> response failed with ${fail}`);
    process.exit(1);
  });

  if (response.statusCode !== 200) {
    console.error(`>> unexpected status code: ${response.statusCode}`);
    process.exit(1);
  }
});

request.on("error", (fail) => {
  console.error(`>> request failed with ${fail}`);
  process.exit(1);
});

request.setTimeout(10000, () => {
  console.error(">> request timeout");
  process.exit(1);
});

request.end();