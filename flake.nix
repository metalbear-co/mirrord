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

        rustToolchain =
          let
            fenix = inputs.fenix.packages.${system};

            toolchainName = {
              name = (pkgs.lib.importTOML ./rust-toolchain.toml).toolchain.channel;
              sha256 = "sha256-ggvRZZFjlAlrZVjqul/f/UpU5CEhDbdKZU0OCR8Uzbc=";
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
          if pkgs.stdenv.isDarwin then
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
        devShells.default = pkgs.mkShell.override { stdenv = pkgs.clangStdenv; } {
          packages =
            with pkgs;
            [
              # Toolchain
              rustToolchain.components
              rustToolchain.rust-analyzer
              rustPlatform.bindgenHook

              # Frontends
              pnpm

              # Integration tests
              cargo-nextest
              go
              nodejs # Install required libraries with `npm install express portfinder --no-save` after entering the devshell.
              (python3.withPackages (
                pypkgs: with pypkgs; [
                  fastapi
                  flask
                  uvicorn
                ]
              ))

              # Release
              python3Packages.towncrier
              cargo-deny
            ]
            ++ lib.optionals stdenv.isLinux [
              # Required to build the agent
              protobuf
            ];

          env =
            with pkgs;
            let
              x86-gcc = lib.getExe pkgsCross.gnu64.stdenv.cc;
            in
            lib.optionalAttrs stdenv.isDarwin {
              # Tells bindgen/cargo which C/C++ toolchain to use when targetting linux
              CC_x86_64_unknown_linux_gnu = x86-gcc;
              CXX_x86_64_unknown_linux_gnu = x86-gcc;
              CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER = x86-gcc;
            };
        };
      }
    );
}
