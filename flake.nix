{
  description = "ccmux";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    crane,
    rust-overlay,
    flake-utils,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [(import rust-overlay)];
        };

        rustToolchainFor = p:
          (p.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml).override {
            extensions = ["rust-src"];
            targets = ["wasm32-unknown-unknown"];
          };
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchainFor;
        src = craneLib.cleanCargoSource ./.;

        commonArgs = {
          inherit src;
          strictDeps = true;
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        cclog-server = craneLib.buildPackage (commonArgs
          // {
            inherit cargoArtifacts;
          });

      in {
        checks = {
          crate = cclog-server;

          clippy = craneLib.cargoClippy (commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            });

          fmt = craneLib.cargoFmt {
            inherit src;
          };

          tests = craneLib.cargoNextest (commonArgs
            // {
              inherit cargoArtifacts;
            });
        };

        packages.default = cclog-server;

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};
          packages = [pkgs.bun pkgs.nodejs pkgs.zig_0_15];
          shellHook = ''
            export NPM_CONFIG_PREFIX="$PWD/.npm-global"
            export PATH="$NPM_CONFIG_PREFIX/bin:$PATH"
            if [ ! -x "$NPM_CONFIG_PREFIX/bin/agent-browser" ]; then
              echo "Installing agent-browser..."
              npm install -g agent-browser >/dev/null 2>&1
            fi
          '';
        };
      }
    );
}
