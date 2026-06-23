{
  pkgs,
  git-hooks,
  system,
  rustToolchain,
}:
  git-hooks.lib.${system}.run {
    src = ../.;
    package = pkgs.prek;
    hooks = {
      biome = {
        enable = true;
        name = "biome";
        description = "Lint and format TypeScript/Svelte in cairn-web.";
        package = pkgs.biome;
        entry = builtins.toString (pkgs.writeShellScript "biome-hook" ''
          check_args="check"
          if [ "''${VALIDATE_FIX:-}" = "1" ]; then check_args="check --write"; fi
          # shellcheck disable=SC2086
          ${pkgs.biome}/bin/biome $check_args
        '');
        files = "^cairn-web/src/.*\\.(ts|js|svelte)$";
        pass_filenames = false;
        require_serial = true;
      };

      cargo-fmt = {
        enable = true;
        name = "cargo-fmt";
        description = "Format Rust code.";
        package = rustToolchain;
        entry = builtins.toString (pkgs.writeShellScript "cargo-fmt-hook" ''
          check_flag="--check"
          if [ "''${VALIDATE_FIX:-}" = "1" ]; then check_flag=""; fi
          # shellcheck disable=SC2086
          ${rustToolchain}/bin/cargo fmt $check_flag
        '');
        files = "\\.rs$";
        pass_filenames = false;
        require_serial = true;
      };

      cargo-clippy = {
        enable = true;
        name = "cargo-clippy";
        description = "Lint Rust code.";
        package = rustToolchain;
        entry = builtins.toString (pkgs.writeShellScript "cargo-clippy-hook" ''
          ${rustToolchain}/bin/cargo clippy --all-targets -- -D warnings
        '');
        files = "\\.rs$";
        pass_filenames = false;
        require_serial = true;
      };

      svelte-build = {
        enable = true;
        name = "svelte-build";
        description = "Verify cairn-web builds without errors.";
        package = pkgs.nodejs;
        entry = builtins.toString (pkgs.writeShellScript "svelte-build-hook" ''
          cd cairn-web
          ${pkgs.nodejs}/bin/npx vite build --logLevel error
        '');
        files = "^cairn-web/src/.*\\.(ts|js|svelte)$";
        pass_filenames = false;
        require_serial = true;
      };
    };
  }
