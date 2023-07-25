import http from "node:http";

const rust_lang = {
  hostname: "www.mirrord-magic-service.dev",
  port: 6666,
  path: "/",
  method: "GET",
};

function makeRequest(options) {
    const request = http.request(options, (response) => {
    console.log(`statusCode: ${response.statusCode}`);

    response.on("data", (data) => {
      console.log(`SUCCESS: response data ${data}`);
    });

    response.on("error", (fail) => {
      console.error(`FAILED: response failed with ${fail}`);
      throw fail;
    });

    if (response.statusCode !== 200) {
      console.warn("Potential failure, might be something wrong with the remote server!");
      throw ">> response.statusCode !== 200";
    }
  });

  request.on("error", (fail) => {
    console.error(`FAILED: request failed with ${fail}`);
    throw fail;
  });

  request.end();
}

makeRequest(rust_lang);
