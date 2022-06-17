console.log(">> starting remote_env test app");

console.log(">> node env vars");

console.log(process.env);

if (process.env.MIRRORD_FAKE_VAR_FIRST !== "mirrord.is.running") {
  process.exit(-1);
}

if (process.env.MIRRORD_FAKE_VAR_SECOND !== "7777") {
  process.exit(-1);
}
