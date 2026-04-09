refactor tests to be structured into check-X, test-X, build-x

- CI was split from a large monolithic .github/workflows/ci.yaml into reusable workflow files like .github/workflows/check-lint.yaml, .github/workflows/test-integration.yaml, .github/
    workflows/test-macos.yaml, .github/workflows/test-windows.yaml, plus composite actions under .github/actions/build-mirrord-cli/action.yml.
- Non-metalbear-co/* third-party GitHub Actions were converted from tag refs to SHA pins, using the latest available tagged releases.
- Non-release Rust workflows were standardized to use a shared host-specific cache key via metalbear-co/setup-rust-toolchain, using mirrord-${{ runner.os }}-${{ runner.arch }}.
- Release Rust jobs were changed to stop using Rust cache, and release Docker image builds were changed so cache usage is configurable via ci/docker-bake.hcl and disabled for final
    release publishes.
- Redundant --target usage was removed where host and target are the same, and the related target/... artifact paths were updated; cross-target cases were left intact.
- Several test workflows now use cargo-nextest instead of plain cargo test for CI execution.
- Windows and Linux release/test jobs were adjusted to use host-default target/debug or target/release paths when not cross-compiling.
