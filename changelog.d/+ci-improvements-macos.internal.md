CI Improvements:
- Unify lint and integration for macOS to save cache and runner.
- Remove trace logging from integration tests on macos
- use node 18 for testing since installing 19 in CI takes hours.
- remove `build_mirrord` job - quite useless as it's used only in other workflow, so have it there and re-use cache
  also save some cache,
- specify target for all cargo invocations to re-use cache efficiently.