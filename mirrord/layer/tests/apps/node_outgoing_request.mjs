import https from "node:https";

const rust_lang = {
  hostname: "www.rust-lang.org",
  port: 443,
  path: "/",
  method: "GET",
};

function makeRequestHttps(options) {
    const request = https.request(options, (response) => {
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

makeRequestHttps(rust_lang);
