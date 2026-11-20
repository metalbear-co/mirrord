{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
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

          # Bazel's prebuilt toolchains expect the dynamic loader at `/lib64/ld-linux-x86-64.so.2`
          # and libraries in `/usr/lib`, which NixOS doesn't have, so on Linux `bazel` runs in an
          # FHS namespace that has both.
          #
          # Bazel's actions and the programs that `bazel run` starts inherit the namespace.
          bazel =
            if pkgs.stdenv.hostPlatform.isLinux then
              pkgs.buildFHSEnv {
                name = "bazel";
                runScript = lib.getExe pkgs.bazelisk;
                # Ctrl-C reaches both bubblewrap and the Bazel client, which asks the Bazel server to
                # cancel the command. If bubblewrap exited on it, the client would die with it, and
                # the server would run the command to its end. So bubblewrap ignores the signal, and
                # waits for the client.
                extraPreBwrapCmds = "trap '' INT";
              }
            else
              pkgs.writeShellScriptBin "bazel" ''exec ${lib.getExe pkgs.bazelisk} "$@"'';
        in
        {
          default = pkgs.mkShell {
            packages =
              with pkgs;
              let
                # Packages required to build the rust workspace
                rustPackages =
                  let
                    rustToolchain =
                      if stdenv.hostPlatform.isDarwin then
                        # On darwin we need the standard library for x86 too in order to compile universal binaries,
                        # as well as the one for linux in order to work on the agent, which is linux-only. nixpkgs'
                        # toolchain only ships the host's standard library, so we use rust-overlay to add the
                        # cross-compilation ones.
                        let
                          rust-bin = inputs.rust-overlay.lib.mkRustBin { } pkgs;
                        in
                        [
                          (rust-bin.stable.latest.default.override {
                            extensions = [ "rust-src" ];
                            targets = [
                              "x86_64-apple-darwin"
                              "x86_64-unknown-linux-gnu"
                            ];
                          })
                          # The wrapper points `RUST_SRC_PATH` to nixpkgs' rust toolchain, but we use rust-overlay's,
                          # so let rust-analyzer find out where it is on its own with `rustc --print sysroot`.
                          rust-analyzer-unwrapped
                        ]
                      else
                        [
                          cargo
                          clippy
                          rustc
                          rustfmt
                          rust-analyzer
                        ];
                  in
                  rustToolchain
                  ++ [
                    rustPlatform.bindgenHook
                    # Required to build `containerd-client`
                    protobuf
                  ];

                # Packages required to build the frontends
                uiPackages = [
                  nodejs
                  pnpm_10
                ];

                # Packages to run tests locally
                testsPackages = [
                  cargo-nextest
                  go
                  (python3.withPackages (
                    pypkgs: with pypkgs; [
                      fastapi
                      flask
                      uvicorn
                    ]
                  ))
                ];

                # Miscellaneous packages to run CI checks locally
                miscPackages = [
                  cargo-deny
                  python3Packages.towncrier
                  # CI's `content_check` runs the skills' fence-tag check through uv, on Python 3.12
                  uv
                  python312
                bazelPackages = [
                  bazel
                ];
              in

            env =
              with pkgs;
              let
                x86-gcc = lib.getExe pkgsCross.gnu64.stdenv.cc;
              in
              lib.optionalAttrs stdenv.hostPlatform.isDarwin {
                # Tells bindgen/cargo which C/C++ toolchain to use when targetting linux
                CC_x86_64_unknown_linux_gnu = x86-gcc;
                CXX_x86_64_unknown_linux_gnu = x86-gcc;
                CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER = x86-gcc;
              };
          };
        }
      );
    };
}
