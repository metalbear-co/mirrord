console.log(">> test_remote_env_vars_panics_when_both_filters_are_specified");

if (process.env.MIRRORD_FAKE_VAR_FIRST || process.env.MIRRORD_FAKE_VAR_SECOND || process.env.MIRRORD_FAKE_VAR_THIRD) {
  process.exit(-1);
} else {
  process.exit(0);
}
