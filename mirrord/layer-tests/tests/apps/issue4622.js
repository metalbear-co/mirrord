process.stdin.resume();

setTimeout(() => {
  require("dns").lookup("example.com", (err) => {
    if (err) {
      console.error(`lookup failed: ${err}`);
      process.exit(1);
    }
    console.log("layer survived");
    process.exit(0);
  });
}, 400);

setTimeout(() => {
  console.error("timed out waiting for lookup");
  process.exit(2);
}, 30000);
