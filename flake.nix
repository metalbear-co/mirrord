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
            components = [
              "cargo"
              "clippy"
              "rust-analyzer"
              "rust-src"
              "rustc"
              "rustfmt"
            ];
          in
          if pkgs.stdenv.isDarwin then
            fenix.combine (
              # Both targets are required to build universal binaries
              builtins.map (triplet: fenix.targets.${triplet}.complete.withComponents components) [
                "x86_64-apple-darwin"
                "aarch64-apple-darwin"
              ]
            )
          else
            fenix.packages.${system}.complete.withComponents components;
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            nightlyRustToolchain

            # Kubernetes
            kubectl
            minikube

            # E2E testing
            go
            nodejs_24 # Needs express.js - install with `npm install express` after entering the devshell
            (python3.withPackages (
              py-pkgs: with py-pkgs; [
                flask
                fastapi
                uvicorn
              ]
            ))
          ];

          MIRRORD_LAYER_FILE_MACOS_ARM64 = "../../../target/aarch64-apple-darwin/debug/libmirrord_layer.dylib";
        };
      }
    );
}
