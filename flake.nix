{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs:
    inputs.flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import inputs.nixpkgs { inherit system; };

        nightlyRustToolchain =
          let
            fenix = inputs.fenix.packages.${system};

            toolchainName = {
              name = (pkgs.lib.importTOML ./rust-toolchain.toml).toolchain.channel;
              sha256 = "sha256-JE+aoEa897IBKa03oVUOOnW+sbyUgXGrhkwzWFzCnnI=";
            };

            toolchain = fenix.fromToolchainName toolchainName;

            components = toolchain.withComponents [
              "cargo"
              "clippy"
              "rust-src"
              "rustc"
              "rustfmt"
            ];

            # Getting rust-analyzer from nixpkgs allows us to update it without updating the toolchain
            rust-analyzer = pkgs.rust-analyzer.override {
              rustSrc = "${toolchain.rust-src}/lib/rustlib/src/rust/library";
            };
          in
          # On darwin we need both x86 and arm toolchains in order to compile universal binaries
          # as well as a linux toolchain in order to work on mirrord-agent, which is linux-only
          if pkgs.stdenv.isDarwin then
            let
              # aarch64 -> x86_64
              # x86_64 -> aarch64
              darwinCrossTarget =
                if pkgs.stdenv.hostPlatform.isAarch then "x86_64-apple-darwin" else "aarch64-apple-darwin";

              crossComponents =
                builtins.map
                  (
                    target:
                    (fenix.targets.${target}.fromToolchainName toolchainName).withComponents [
                      # Fewer components are required for the cross-compilation toolchains because we don't need IDE functionality
                      "rustc"
                      "rust-src"
                    ]
                  )
                  [
                    darwinCrossTarget
                    "x86_64-unknown-linux-gnu"
                  ];
            in
            {
              components = fenix.combine ([ components ] ++ crossComponents);
              inherit rust-analyzer;
            }
          else
            {
              inherit components rust-analyzer;
            };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            nightlyRustToolchain.components
            nightlyRustToolchain.rust-analyzer
            cargo-zigbuild

            # E2E testing
            go
            protobuf
            clang
            rustPlatform.bindgenHook
            nodejs_24 # Needs express.js - install with `npm install express` after entering the devshell
            (python3.withPackages (
              py-pkgs: with py-pkgs; [
                flask
                fastapi
                uvicorn
              ]
            ))
          ];

          env =
            with pkgs;
            lib.optionalAttrs stdenv.isDarwin {
              # Tells bindgen which C compiler to compile frida with when targetting linux
              CC_x86_64_unknown_linux_gnu = lib.getExe pkgsCross.gnu64.stdenv.cc;
            };
        };
      }
    );
}
