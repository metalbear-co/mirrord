console.log(">> starting remote_env test app");

console.log(">> node env vars");

console.log(process.env);

if (
  process.env.MIRRORD_FAKE_VAR_FIRST === "mirrord.is.running" &&
  process.env.MIRRORD_FAKE_VAR_SECOND === undefined
) {
  process.exit(0);
} else {
  process.exit(-1);
}
