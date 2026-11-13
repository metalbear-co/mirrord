{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs =
    inputs:
    let
      inherit (inputs.nixpkgs) lib;
    in
    {
      devShells = lib.genAttrs lib.systems.flakeExposed (
        system:
        let
          pkgs = inputs.nixpkgs.legacyPackages.${system};

          # The `bazel` command runs bazelisk, which runs the Bazel release that `.bazelversion`
          # names, as CI does, so that the devshell and CI share cached results.
          #
          # Bazel downloads prebuilt toolchains, such as rustc, Node.js and Python, which load their
          # libraries through `/lib64/ld-linux-x86-64.so.2` and look for them in `/usr/lib`, as
          # Bazel's own release does. NixOS has neither, so on Linux the `bazel` command runs
          # bazelisk in a bubblewrap namespace that provides both, with the libraries below. Every
          # action and `bazel run` target runs in that namespace too, because the Bazel server and
          # the programs that it starts inherit it.
          bazel =
            if pkgs.stdenv.hostPlatform.isLinux then
              pkgs.buildFHSEnv {
                name = "bazel";
                targetPkgs = pkgs: [
                  # libgcc_s and libstdc++.
                  pkgs.stdenv.cc.cc.lib
                  pkgs.zlib
                  # Tests run with Bazel's default `PATH`, `/bin:/usr/bin:/usr/local/bin`, and some
                  # of them run `git`, which other Linux machines have there.
                  pkgs.git
                ];
                runScript = lib.getExe pkgs.bazelisk;
              }
            else
              pkgs.writeShellScriptBin "bazel" ''exec ${lib.getExe pkgs.bazelisk} "$@"'';

          # The tools that `bazel run //:lint` and `bazel run //:format` take from `PATH`.
          lintPackages = with pkgs; [
            markdownlint-cli2
            typos
            vale
            cargo-deny
            cargo-shear
            python3Packages.towncrier
            bazel-buildtools
            ruff
            ty
            # The flaky test report's tests run `.github/scripts/flaky-tests.sh`, which unzips the
            # reports.
            unzip
            # pnpm installs the frontends' ESLint, TypeScript and Prettier.
            nodejs
            pnpm_10
            # `bazel run //mirrord/mcp/vendor -- check` runs the skills repo's config check through
            # uv, on the Python version that the skills repo's CI uses.
            uv
            python312
          ];
        in
        {
          default = pkgs.mkShell {
            packages =
              with pkgs;
              let
                # The Rust toolchain, and the tools that the build scripts need, for rust-analyzer's
                # `cargo check`. Bazel brings its own toolchains, and the tools that the tests run,
                # like Go, Python and cargo-nextest.
                rustPackages = [
                  cargo
                  clippy
                  rustc
                  rustfmt
                  rust-analyzer
                  rustPlatform.bindgenHook
                  # Required to build `containerd-client`
                  protobuf
                  # Required to build `rdkafka-sys`, which the end-to-end tests' Kafka consumer uses
                  cmake
                ];

                # Bazel builds and tests the workspace with toolchains that it downloads itself
                # (see `MODULE.bazel`).
                bazelPackages = [
                  bazel
                  starpls
                ];
              in
              rustPackages ++ bazelPackages ++ lintPackages;
          };

          # CI's lint job runs `bazel run //:lint` and `bazel run //:format` in this shell, so that
          # it runs the tools at the versions that developers run, without the toolchains that the
          # default shell builds the workspace with. Bazel itself comes from CI's own setup.
          lint = pkgs.mkShellNoCC {
            packages = lintPackages ++ [
              # cargo-deny and cargo-shear read the workspace through `cargo metadata`.
              pkgs.cargo
            ];
          };
        }
      );
    };
}
