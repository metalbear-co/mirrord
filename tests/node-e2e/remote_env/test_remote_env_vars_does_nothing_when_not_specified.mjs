console.log(">> starting remote_env test app");

console.log(">> node env vars");

console.log(process.env);

if (process.env.MIRRORD_FAKE_VAR_FIRST || process.env.MIRRORD_FAKE_VAR_SECOND) {
  process.exit(-1);
} else {
  process.exit(0);
}
