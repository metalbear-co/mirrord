macOS CI Improvements:
- Unify lint and integration for macOS to save cache and runner.
- Remove trace logging from integration tests on macos
- use node 18 for testing since installing 19 in CI takes hours.