//! Build a `cairn_pty::SpawnOptions` from a wire `session-spec`.

use cairn_pty::SpawnOptions;
use cairn_protocol::cairn::daemon::types::SessionSpec;

/// Translate a `session-spec` into spawn options. An empty `command` falls
/// back to `default_shell`. `env-inherit=false` clears the inherited env.
pub fn options_from(spec: SessionSpec, default_shell: &str) -> SpawnOptions {
    let mut argv = spec.command.into_iter();
    let program = argv.next().unwrap_or_else(|| default_shell.to_string());

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(argv);
    if !spec.env_inherit {
        cmd.env_clear();
    }
    for (k, v) in spec.env {
        cmd.env(k, v);
    }
    if let Some(dir) = spec.workdir {
        cmd.current_dir(dir);
    }

    SpawnOptions::new(cmd).with_scrollback_lines(spec.scrollback_lines as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_protocol::cairn::daemon::types::SessionSpec;

    fn base_spec() -> SessionSpec {
        SessionSpec {
            name: None, command: vec![], env: vec![], env_inherit: true,
            workdir: None, tty: true, stdin: true, idle_timeout_secs: None,
            scrollback_lines: 500,
        }
    }

    #[test]
    fn empty_command_uses_default_shell() {
        let opts = options_from(base_spec(), "/bin/zsh");
        let std = opts.command.as_std();
        assert_eq!(std.get_program(), std::ffi::OsStr::new("/bin/zsh"));
        assert_eq!(opts.scrollback_lines, 500);
    }

    #[test]
    fn explicit_command_and_env_are_applied() {
        let mut spec = base_spec();
        spec.command = vec!["echo".into(), "hi".into()];
        spec.env = vec![("FOO".into(), "bar".into())];
        spec.workdir = Some("/tmp".into());
        let opts = options_from(spec, "/bin/sh");
        let std = opts.command.as_std();
        assert_eq!(std.get_program(), std::ffi::OsStr::new("echo"));
        let args: Vec<_> = std.get_args().collect();
        assert_eq!(args, vec![std::ffi::OsStr::new("hi")]);
        assert_eq!(std.get_current_dir(), Some(std::path::Path::new("/tmp")));
    }
}
