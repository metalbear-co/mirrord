console.log(">> test_remote_env_vars_exclude_works");

if (
  process.env.MIRRORD_FAKE_VAR_FIRST === undefined &&
  process.env.MIRRORD_FAKE_VAR_SECOND === "7777" &&
  process.env.MIRRORD_FAKE_VAR_THIRD === "foo=bar"
) {
  process.exit(0);
} else {
  process.exit(-1);
}
