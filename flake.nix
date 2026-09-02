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

          rustToolchain =
            let
              fenix = inputs.fenix.packages.${system};

              toolchainName = {
                name = (lib.importTOML ./rust-toolchain.toml).toolchain.channel;
                sha256 = "sha256-Ki4L7dIE4vXNJE2vTI+REJQ/cYSehBASKPocAFeDkQk=";
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
            # as well as a linux toolchain in order to work on the agent, which is linux-only
            if pkgs.stdenv.hostPlatform.isDarwin then
              let
                crossComponents =
                  map
                    (
                      target:
                      (fenix.targets.${target}.fromToolchainName toolchainName).withComponents [
                        # Fewer components are required for the cross-compilation toolchains because we don't need IDE functionality
                        "rustc"
                        "rust-src"
                      ]
                    )
                    [
                      "x86_64-apple-darwin"
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
          default = pkgs.mkShell.override { stdenv = pkgs.clangStdenv; } {
            packages = with pkgs; [
              # Toolchain
              rustToolchain.components
              rustToolchain.rust-analyzer
              rustPlatform.bindgenHook
              protobuf # Required by `containerd-client`

              # Frontends
              nodejs
              pnpm

              # Integration tests
              cargo-nextest
              go
              (python3.withPackages (
                pypkgs: with pypkgs; [
                  fastapi
                  flask
                  uvicorn
                ]
              ))

              # CI stuff
              python3Packages.towncrier
              cargo-deny
            ];

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
