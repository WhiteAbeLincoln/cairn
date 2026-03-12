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

        rodney = pkgs.buildGoModule {
          pname = "rodney";
          version = "0.5.0";

          # Fork with `rodney viewport` command for persistent viewport resizing
          # https://github.com/simonw/rodney/pull/33
          src = pkgs.fetchFromGitHub {
            owner = "matthewbjones";
            repo = "rodney";
            rev = "d1281d2a0f5d36b1eccf26f08811604b1118b373";
            hash = "sha256-x39Y51rvJdMBFItkPoo7B4UGFRi6B9W8trrNzLFOv0I=";
          };

          vendorHash = "sha256-h4U43W3hLoF+p25/jNRaW8okeEzAZQEmKtwB5l4kGW4=";

          # Tests require a running Chrome instance
          doCheck = false;

          # Remove --single-process flag that crashes on macOS
          # https://github.com/simonw/rodney/issues/9
          postPatch = ''
            substituteInPlace main.go \
              --replace-fail 'Set("single-process").' ""
          '';
        };
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
          # TODO: move to the web-server app
          packages = [pkgs.bun rodney];
        };
      }
    );
}
