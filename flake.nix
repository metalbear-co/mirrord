{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    fenix = {
      url = "github:nix-community/fenix";
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
        in
        {
          default = pkgs.mkShell {
            packages =
              with pkgs;
              let
                # Packages required to build the rust workspace
                rustPackages =
                  let
                    rust-toolchain =
                      let
                        fenix = inputs.fenix.packages.${system};

                        toolchainName = {
                          name = "nightly-2026-08-13";
                          sha256 = "sha256-Ki4L7dIE4vXNJE2vTI+REJQ/cYSehBASKPocAFeDkQk=";
                        };

                        components = with fenix.fromToolchainName toolchainName; [
                          cargo
                          clippy
                          rust-src
                          rustc
                          rustfmt
                        ];

                        # On darwin we need the standard library for x86 too in order to compile universal binaries,
                        # as well as the one for linux in order to work on the agent, which is linux-only
                        crossComponents =
                          let
                            crossTargets = lib.optionals stdenv.hostPlatform.isDarwin [
                              "x86_64-apple-darwin"
                              "x86_64-unknown-linux-gnu"
                            ];
                          in
                          lib.map (target: (fenix.targets.${target}.fromToolchainName toolchainName).rust-std) crossTargets;
                      in
                      fenix.combine (components ++ crossComponents);
                  in
                  [
                    rust-toolchain
                    # The wrapper points `RUST_SRC_PATH` to nixpkgs' rust toolchain, but we use a custom toolchain,
                    # so let rust-analyzer find out where it is on its own with `rustc --print sysroot`.
                    rust-analyzer-unwrapped
                    rustPlatform.bindgenHook
                    # Required to build `containerd-client`
                    protobuf
                  ];

                # Packages required to build the frontends
                uiPackages = [
                  nodejs
                  pnpm
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
                ];
              in
              rustPackages ++ uiPackages ++ testsPackages ++ miscPackages;

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
