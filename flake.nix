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

        # Pinned ghostty source for libghostty-vt-sys 0.1.1. The rev matches
        # GHOSTTY_COMMIT in that crate's build.rs. Pointing GHOSTTY_SOURCE_DIR
        # at this nix-store path lets the build skip its runtime `git clone`
        # of ghostty.
        ghosttySrc = pkgs.fetchFromGitHub {
          owner = "ghostty-org";
          repo = "ghostty";
          rev = "bebca84668947bfc92b9a30ed58712e1c34eee1d";
          hash = "sha256-7MPEjIAQD+Z/zdP4h/yslysuVnhCESOPvdvwoLoPVmI=";
        };

        commonArgs = {
          inherit src;
          strictDeps = true;
          GHOSTTY_SOURCE_DIR = "${ghosttySrc}";
          nativeBuildInputs = [pkgs.zig_0_15];
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
            # Make libghostty-vt-sys use the nix-prefetched ghostty source
            # instead of git-cloning at build time.
            export GHOSTTY_SOURCE_DIR="${ghosttySrc}"
            # Zig needs writable cache dirs. libghostty-vt-sys runs zig with
            # cwd set to GHOSTTY_SOURCE_DIR (read-only nix store path), so
            # zig's default local cache `./.zig-cache` is unwritable. Pin
            # both global and local caches under $HOME.
            export ZIG_GLOBAL_CACHE_DIR="$HOME/.cache/zig"
            export ZIG_LOCAL_CACHE_DIR="$HOME/.cache/zig-local"
            mkdir -p "$ZIG_GLOBAL_CACHE_DIR" "$ZIG_LOCAL_CACHE_DIR"
            if [ ! -x "$NPM_CONFIG_PREFIX/bin/agent-browser" ]; then
              echo "Installing agent-browser..."
              npm install -g agent-browser >/dev/null 2>&1
            fi
          '';
        };
      }
    );
}
