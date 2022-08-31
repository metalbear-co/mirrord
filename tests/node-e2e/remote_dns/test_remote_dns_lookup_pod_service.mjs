const dns = require("node:dns");
const dnsPromises = dns.promises;

console.log(">> test_remote_remote_dns_lookup_pod_service");

dnsPromises
  .lookup("nginx")
  .then((resolved) => {
    console.log(">> resolved ", resolved);

    assert(
      resolved.address !== "255.127.0.0",
      ">> Trying to resolve pod failed with an invalid address ",
      resolved
    );

    process.exit(0);
  })
  .catch((fail) => {
    console.error(">> failed with ", fail);

    process.exit(-1);
  });
