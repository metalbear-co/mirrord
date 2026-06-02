import https from "node:https";

const AVAILABLE_HOSTS = process.env.AVAILABLE_HOSTS;

// We hit servers we don't control, so an occasional failure is expected and
// shouldn't fail the test. We only fail if more than this many requests fail.
const MAX_ALLOWED_FAILURES = 2;

function makeRequest(host) {
  return new Promise((resolve) => {
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

      response.on("end", () => {
        resolve(true);
      });

      response.on("error", (error) => {
        console.error(`response from ${host} failed: ${error}`);
        resolve(false);
      });
    });

    request.on("error", (error) => {
      console.error(`request to ${host} failed: ${error}`);
      resolve(false);
    });

    request.end();
  });
}

if (AVAILABLE_HOSTS === undefined) {
  console.error("`AVAILABLE_HOSTS` environment variable is missing");
  process.exit(1);
}

const hosts = AVAILABLE_HOSTS.split(",");
const results = await Promise.all(hosts.map(makeRequest));
const failures = results.filter((succeeded) => !succeeded).length;

console.log(`${failures} out of ${hosts.length} requests failed`);

if (failures > MAX_ALLOWED_FAILURES) {
  console.error(
    `${failures} requests failed, more than the allowed ${MAX_ALLOWED_FAILURES}`
  );
  process.exit(1);
}
