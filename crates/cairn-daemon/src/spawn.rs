//! Build a `cairn_pty::SpawnOptions` from a wire `session-spec`.

use cairn_protocol::cairn::daemon::types::SessionSpec;
use cairn_pty::SpawnOptions;

use crate::http_proxy::ProxyEnvironment;

/// Translate a `session-spec` into spawn options. An empty `command` falls
/// back to `default_shell`. `env-inherit=false` clears the inherited env.
pub fn options_from(
    spec: SessionSpec,
    default_shell: &str,
    session_id: String,
    proxy: Option<&ProxyEnvironment>,
) -> SpawnOptions {
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
    if let Some(proxy) = proxy {
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            cmd.env(key, &proxy.proxy_url);
        }
        cmd.env("NODE_EXTRA_CA_CERTS", &proxy.ca_path);
        for key in [
            "SSL_CERT_FILE",
            "REQUESTS_CA_BUNDLE",
            "CURL_CA_BUNDLE",
            "GIT_SSL_CAINFO",
        ] {
            cmd.env(key, &proxy.bundle_path);
        }
    }

    SpawnOptions::new(cmd)
        .with_scrollback_lines(spec.scrollback_lines as usize)
        .with_session_id(session_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_protocol::cairn::daemon::types::SessionSpec;

    fn base_spec() -> SessionSpec {
        SessionSpec {
            name: None,
            command: vec![],
            env: vec![],
            env_inherit: true,
            workdir: None,
            tty: true,
            stdin: true,
            idle_timeout_secs: None,
            scrollback_lines: 500,
            http_proxy: None,
        }
    }

    #[test]
    fn empty_command_uses_default_shell() {
        let opts = options_from(base_spec(), "/bin/zsh", String::new(), None);
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
        let opts = options_from(spec, "/bin/sh", "test-id".to_string(), None);
        let std = opts.command.as_std();
        assert_eq!(std.get_program(), std::ffi::OsStr::new("echo"));
        let args: Vec<_> = std.get_args().collect();
        assert_eq!(args, vec![std::ffi::OsStr::new("hi")]);
        assert_eq!(std.get_current_dir(), Some(std::path::Path::new("/tmp")));
        assert_eq!(opts.session_id, "test-id");
    }

    #[test]
    fn proxy_environment_overrides_child_proxy_and_trust_variables() {
        let mut spec = base_spec();
        spec.env = vec![("HTTPS_PROXY".into(), "http://old.example:8080".into())];
        let proxy = ProxyEnvironment {
            proxy_url: "http://127.0.0.1:43210".into(),
            ca_path: "/tmp/cairn-ca.pem".into(),
            bundle_path: "/tmp/cairn-bundle.pem".into(),
        };
        let opts = options_from(spec, "/bin/sh", "test-id".into(), Some(&proxy));
        let env: std::collections::HashMap<_, _> = opts
            .command
            .as_std()
            .get_envs()
            .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value.to_owned())))
            .collect();
        assert_eq!(
            env.get(std::ffi::OsStr::new("HTTPS_PROXY")),
            Some(&std::ffi::OsString::from("http://127.0.0.1:43210"))
        );
        assert_eq!(
            env.get(std::ffi::OsStr::new("NODE_EXTRA_CA_CERTS")),
            Some(&std::ffi::OsString::from("/tmp/cairn-ca.pem"))
        );
        assert_eq!(
            env.get(std::ffi::OsStr::new("SSL_CERT_FILE")),
            Some(&std::ffi::OsString::from("/tmp/cairn-bundle.pem"))
        );
    }
}
