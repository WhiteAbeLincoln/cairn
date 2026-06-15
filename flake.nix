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
    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    crane,
    rust-overlay,
    flake-utils,
    git-hooks,
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
            targets = [];
          };
        rustToolchain = rustToolchainFor pkgs;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchainFor;
        src = craneLib.cleanCargoSource ./.;

        pre-commit = import ./nix/hooks.nix {
          inherit pkgs git-hooks system rustToolchain;
        };

        validate-changes = pkgs.writeShellApplication {
          name = "validate-changes";
          text = builtins.readFile ./nix/validate-changes.sh;
          runtimeInputs = pre-commit.enabledPackages;
        };

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
          pname = "cairn";
          strictDeps = true;
          GHOSTTY_SOURCE_DIR = "${ghosttySrc}";
          nativeBuildInputs = [pkgs.zig_0_15];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        buildPkg = pname:
          craneLib.buildPackage (commonArgs
            // {
              inherit pname cargoArtifacts;
              cargoExtraArgs = "-p ${pname}";
            });

        cairn = buildPkg "cairn";
        cairn-daemon = buildPkg "cairn-daemon";
      in {
        checks = {
          inherit pre-commit cairn cairn-daemon;

          clippy = craneLib.cargoClippy (commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            });

          fmt = craneLib.cargoFmt {
            inherit src;
            inherit (commonArgs) pname;
          };

          tests = craneLib.cargoNextest (commonArgs
            // {
              inherit cargoArtifacts;
            });
        };

        packages = {
          default = cairn;
          inherit cairn cairn-daemon validate-changes;
        };

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};
          packages = [pkgs.biome pkgs.bun pkgs.deno pkgs.nodejs pkgs.zig_0_15 validate-changes] ++ pre-commit.enabledPackages;
          shellHook =
            pre-commit.shellHook
            + ''
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
