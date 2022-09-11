console.log(">> test_remote_env_vars_include_works");

if (
  process.env.MIRRORD_FAKE_VAR_FIRST === "mirrord.is.running" &&
  process.env.MIRRORD_FAKE_VAR_SECOND === undefined &&
  process.env.MIRRORD_FAKE_VAR_THIRD === undefined
) {
  process.exit(0);
} else {
  process.exit(-1);
}
