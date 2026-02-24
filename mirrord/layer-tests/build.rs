/// By default this crate enables the `build-artifacts` feature, which
/// pulls in the build-dependencies for `mirrord` to populate
/// `MIRRORD_TESTS_USE_BINARY` from the artifact computed path.
/// In CI you can disable `build-artifacts` and
/// provide the env var to avoid costly redundant rebuilds.
fn main() {
    // assert binary mirrord_tests::utils::run_command::run_mirrord
    option_env!("CARGO_BIN_FILE_MIRRORD")
        .or_else(|| option_env!("MIRRORD_TESTS_USE_BINARY"))
        .expect("Mirrord Binary is required, either provide MIRRORD_TESTS_USE_BINARY or permit layer-tests default features");
}
