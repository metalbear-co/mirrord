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

          # Bazel's prebuilt toolchains expect the dynamic loader at `/lib64/ld-linux-x86-64.so.2`
          # and libraries in `/usr/lib`, which NixOS doesn't have, so on Linux `bazel` runs in an
          # FHS namespace that has both.
          #
          # Bazel's actions and the programs that `bazel run` starts inherit the namespace.
          bazel =
            if pkgs.stdenv.hostPlatform.isLinux then
              pkgs.buildFHSEnv {
                name = "bazel";
                targetPkgs =
                  pkgs: with pkgs; [
                    stdenv.cc.cc.lib
                    zlib
                    # Some tests run `git` from Bazel's default `PATH`.
                    git
                    # rules_powershell's build actions run PowerShell, which needs ICU.
                    icu
                  ];
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
          # An optional shell for the developers who use nix. Bazel downloads every tool that the
          # build, `bazel run //:lint`, `bazel run //:format` and the end-to-end tests run, so the
          # shell only has Bazel and the Rust tools of rust-analyzer.
          default = pkgs.mkShell {
            packages =
              with pkgs;
              let
                # For rust-analyzer's `cargo check`. Bazel brings its own toolchains.
                rustPackages = [
                  cargo
                  clippy
                  rustc
                  rustfmt
                  rust-analyzer
                  rustPlatform.bindgenHook
                  # Required to build `containerd-client`
                  protobuf
                ];

                bazelPackages = [
                  bazel
                ];
              in
              rustPackages ++ bazelPackages;
          };
        }
      );
    };
}
