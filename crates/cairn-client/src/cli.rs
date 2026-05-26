use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use clap::{ArgAction, Parser};

#[derive(Parser, Debug)]
#[command(version, author, about)]
pub struct Cli {
    /// Daemon endpoint URI; selects both the transport and the address.
    ///
    /// `unix:///path/to/cairn.sock` connects to a local daemon over a
    /// unix socket. The daemon authenticates the client via
    /// `SO_PEERCRED` plus filesystem permissions on the socket, so
    /// `--token` is not consulted for this transport.
    ///
    /// `ws://host:port` and `wss://host:port` connect to a remote
    /// daemon over (TLS-secured) websockets. `--token` (or
    /// `CAIRN_TOKEN`) is required for these transports.
    ///
    /// If unset, defaults to the platform-standard local socket
    /// (`$XDG_RUNTIME_DIR/cairn/cairn.sock` on Linux,
    /// `$TMPDIR/cairn/cairn.sock` otherwise).
    #[clap(long, env = "CAIRN_DAEMON", global = true)]
    pub daemon: Option<String>,

    /// Bearer token for authenticating to a remote daemon.
    ///
    /// Ignored when `--daemon` points at a unix socket. Prefer the
    /// `CAIRN_TOKEN` environment variable over the command-line flag
    /// so the token doesn't appear in shell history or `ps` output;
    /// for the same reason `--help` won't echo the env var's value.
    #[clap(long, env = "CAIRN_TOKEN", global = true, hide_env_values = true)]
    pub token: Option<String>,

    /// Increase log verbosity. Repeat for more detail: `-v` enables
    /// info, `-vv` debug, `-vvv` trace. Default is warn-level.
    #[clap(long, short = 'v', action = ArgAction::Count, global = true)]
    pub verbose: u8,

    /// When to emit color in client output.
    ///
    /// `auto` (the default) emits color when stdout is a TTY and the
    /// `NO_COLOR` environment variable is unset, matching the
    /// `no-color.org` convention.
    #[clap(long, value_enum, global = true, default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Parser, Debug)]
pub enum Command {
    /// Run a command in a new PTY session.
    ///
    /// Mirrors `docker exec` semantics: pass `-i` to keep stdin open
    /// and `-t` to allocate a TTY in the new session. Without these,
    /// the child runs non-interactively — fine for one-shot commands
    /// but most shells/TUIs will refuse.
    ///
    /// For the typical "open a shell or launch a TUI" case, use
    /// `cairn run` instead — same flags, but `-it` is on by default.
    Exec(ExecArgs),
    /// Run a command in a new PTY session, interactively.
    ///
    /// Equivalent to `cairn exec -it`: `-i`/`--interactive` and
    /// `-t`/`--tty` are on by default. Pass `--no-interactive` or
    /// `--no-tty` to disable either. All other flags are identical
    /// to `cairn exec`.
    Run(ExecArgs),
    /// Attach the client's terminal to a session, forwarding input
    /// and resize events.
    ///
    /// Requires the client to have an interactive terminal.
    Attach {
        #[command(flatten)]
        session: SessionTarget,
        /// Don't forward client stdin to the session. The session's
        /// output still streams to the client's terminal, but
        /// keystrokes (and pasted input) are dropped.
        ///
        /// Mirrors `docker attach --no-stdin`. Logically the inverse
        /// of `cairn run`/`cairn exec`'s `-i/--interactive` flag:
        /// attach implies interactive, this opts out.
        #[clap(long)]
        no_stdin: bool,
        /// Key sequence that detaches the client from the session
        /// without killing the underlying process.
        ///
        /// Format is comma-separated key tokens, e.g. `ctrl-q,ctrl-q`
        /// or `ctrl-a,d` (tmux-style). If unset, defaults to
        /// `ctrl-q,ctrl-q`.
        #[clap(long)]
        detach_keys: Option<String>,
        /// Forward signals received by the client to the attached
        /// session's process group. Does not apply to SIGCHLD,
        /// SIGSTOP, or SIGKILL.
        ///
        /// With `--sig-proxy=true` (the default), a Ctrl-C in the
        /// client sends SIGINT to the session. With `--sig-proxy=false`,
        /// only the client receives the signal — useful when you want
        /// the session to survive a client disconnect without being
        /// asked to clean up.
        #[clap(
            long,
            action = ArgAction::Set,
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "true",
            default_value_t = true,
        )]
        sig_proxy: bool,
    },
    /// Stream session output to stdout. Multi-session safe.
    ///
    /// By default, prints the buffered output and exits immediately.
    /// Pass `--follow` to keep streaming live output until the session
    /// exits.
    Logs {
        #[command(flatten)]
        sessions: SessionTargets,
        /// Strip ANSI control sequences from the output, leaving only
        /// raw text.
        #[clap(long)]
        strip: bool,
        /// Prefix each line with its originating session's name or uuid.
        /// Useful when streaming multiple sessions interleaved.
        #[clap(long)]
        prefix: bool,
        /// Follow live output: keep streaming as the session produces
        /// more, exiting when the session does. Without this, the
        /// buffered output is dumped and the command exits immediately.
        #[clap(long, short = 'f')]
        follow: bool,
        /// Limit output to the last N lines of buffered scrollback.
        #[clap(long, short = 'n')]
        tail: Option<usize>,
    },
    /// List active PTY sessions.
    //
    // TODO(cli/list-design): filters and machine-readable output need
    // more design before we wire them up. Likely shape:
    //   - Repeatable `--filter key=value` (e.g.
    //     `--filter status=running --filter name=build-*`), matching
    //     docker/kubectl conventions.
    //   - Single `--output {plain,json,jsonl,wide}` flag for output
    //     format; `jsonl` for streaming pipelines, `wide` for an
    //     extended column set.
    // The `--output` design should also cover `inspect` and any other
    // command that returns structured data, so settle on a shared
    // value-enum (likely `OutputFormat`) when designing.
    #[clap(visible_alias = "ls")]
    List,
    /// Send characters to a session. Input is read from stdin if no
    /// argument is given.
    Send {
        #[command(flatten)]
        session: SessionTarget,
        /// If set and input is given as an argument, sends the input
        /// string as-is rather than appending a newline. Piped input
        /// from stdin is always sent raw, regardless of this flag.
        #[arg(long, short, default_value_t = false)]
        raw: bool,
        /// The string to send as input to the session.
        /// If not given, reads from stdin until EOF.
        #[arg(trailing_var_arg = true)]
        input: Vec<String>,
        // TODO: do we ever want to send raw bytes that aren't valid UTF-8?
    },
    /// Kill one or more sessions.
    ///
    /// By default, blocks until each targeted session has actually
    /// exited and been reaped. Pass `--no-wait` to fire-and-forget,
    /// or `--timeout` for graceful-then-force semantics (send the
    /// requested signal, wait the specified duration, escalate to
    /// SIGKILL if still alive).
    Kill {
        /// Signal to send instead of the default TERM.
        ///
        /// Accepts a symbolic name (with or without the `SIG` prefix,
        /// case-insensitive — e.g. `INT`, `SIGINT`, `sigint`) or a raw
        /// signal number in `1..=255` as an escape hatch. The daemon is
        /// responsible for validating that the number is meaningful on
        /// its OS.
        #[clap(long, short, default_value_t, value_parser = Signal::from_str)]
        signal: Signal,
        /// Don't wait for the session(s) to actually exit; return as
        /// soon as the signal has been dispatched. Mutually exclusive
        /// with `--timeout`.
        #[clap(long, conflicts_with = "timeout")]
        no_wait: bool,
        /// Wait up to this duration for the session to exit, then
        /// escalate to SIGKILL if it's still alive. Implies the
        /// default wait behavior.
        ///
        /// Accepts humantime durations like `5s`, `30s`, `1m`.
        #[clap(long, value_parser = humantime::parse_duration)]
        timeout: Option<Duration>,
        #[command(flatten)]
        sessions: SessionTargets,
    },
    /// Block until a session exits, propagating its exit code as the
    /// client's exit code.
    ///
    /// Useful for scripting `cairn spawn --detach …; cairn wait <name>`
    /// patterns.
    Wait {
        #[command(flatten)]
        session: SessionTarget,
        /// Maximum time to wait before giving up. If exceeded, the
        /// client exits with a distinct non-zero code without killing
        /// the session.
        #[clap(long, short = 't', value_parser = humantime::parse_duration)]
        timeout: Option<Duration>,
    },
    /// Re-spawn a session's child process using its original argv,
    /// env, and cwd. The session keeps its name and uuid.
    Restart {
        #[command(flatten)]
        session: SessionTarget,
        /// Restart even if the current child is still running. Without
        /// this, restarting a live session is an error.
        #[clap(long)]
        force: bool,
    },
    /// Rename a session.
    ///
    /// Example: `cairn rename build-old --to build-new`
    Rename {
        #[command(flatten)]
        session: SessionTarget,
        /// New name for the session. Must be unique across active sessions.
        #[clap(long = "to", required = true)]
        new_name: String,
    },
    /// Show all known metadata for a session: pid, dimensions,
    /// attached clients, creation time, spawn arguments, etc.
    //
    // TODO(cli/inspect-output): a `--output {plain,json,yaml}` flag
    // here should share its value-enum with the `list` machine-
    // readable output design above.
    Inspect {
        #[command(flatten)]
        session: SessionTarget,
    },
    /// Detach attached clients from one or more sessions without
    /// killing the underlying process.
    ///
    /// By default, kicks every attached client. Use `--client` to
    /// target a specific one (look up its id with `cairn inspect`).
    #[clap(visible_alias = "detach")]
    Kick {
        #[command(flatten)]
        sessions: SessionTargets,
        /// Only kick the named client; others stay attached.
        #[clap(long)]
        client: Option<String>,
    },
    /// Print the identity the daemon authenticated this client as.
    ///
    /// Doubles as a connection diagnostic: a successful response
    /// confirms the daemon is reachable and credentials are valid.
    Whoami,
    /// Print client and daemon versions side by side.
    ///
    /// Useful for confirming protocol compatibility. `cairn --version`
    /// only prints the client version; this subcommand additionally
    /// queries the daemon.
    Version,
    /// Generate a shell completion script and print it to stdout.
    ///
    /// Pipe the output into your shell's completion loader, e.g.
    /// `cairn completion bash > ~/.local/share/bash-completion/completions/cairn`
    /// or `cairn completion zsh > ${fpath[1]}/_cairn`.
    Completion {
        /// Target shell.
        shell: clap_complete::Shell,
    },
}

/// Selector for a single session.
///
/// Exactly one of: a positional name/uuid, or `--latest`. Glob
/// patterns are intentionally not accepted by single-session commands
/// — a glob could resolve to multiple sessions, which these commands
/// can't handle unambiguously. Use a literal name or `--latest`; if
/// you need bulk operations, see the commands that accept
/// [`SessionTargets`].
#[derive(clap::Args, Debug)]
#[group(required = true, multiple = false)]
pub struct SessionTarget {
    /// Name or uuid of the session.
    pub session: Option<SessionId>,
    /// Operate on the most recently created session.
    #[clap(long, short = 'l')]
    pub latest: bool,
}

/// Selector for one or more sessions.
///
/// Provide one of: a list of names/uuids/glob patterns, `--latest`,
/// or `--all`. Glob patterns (those containing `*`, `?`, or `[`) are
/// matched against session names by the daemon; literal names and
/// uuids are matched exactly.
#[derive(clap::Args, Debug)]
#[group(required = true, multiple = false)]
pub struct SessionTargets {
    /// Names, uuids, or glob patterns of sessions to operate on.
    #[clap(num_args = 1..)]
    pub sessions: Vec<SessionId>,
    /// Operate on the most recently created session.
    #[clap(long, short = 'l')]
    pub latest: bool,
    /// Operate on all sessions.
    #[clap(long)]
    pub all: bool,
}

/// Shared arguments for `cairn exec` and `cairn run`.
///
/// The two commands carry identical fields and differ only in the
/// default values used for `-i`/`-t` when neither the positive nor
/// negative flag is supplied — see [`Self::interactive_with_default`]
/// and [`Self::tty_with_default`]. Resolution is deferred to dispatch
/// rather than baked into clap because two subcommands can't carry
/// different defaults for the same field on the same `Args` struct.
#[derive(clap::Args, Debug)]
pub struct ExecArgs {
    /// Human-friendly session name. If omitted, the daemon assigns
    /// a default. Must be unique across active sessions. The name is
    /// the correlation key for tying a session to an external system
    /// (e.g. a ticket id); the session's UUIDv7 id is always assigned
    /// by the daemon.
    #[clap(long, short = 'n')]
    pub name: Option<String>,
    /// Run the session detached: the daemon creates it and the
    /// client returns immediately. The session's TTY and stdin
    /// allocation are independent of this flag — `cairn run -d`
    /// still defaults to `-it`, so `cairn attach <name>` can
    /// connect interactively later.
    #[clap(long, short = 'd')]
    pub detach: bool,
    /// Idle timeout: the daemon kills the session if it has had no
    /// attached clients for the specified duration.
    ///
    /// Accepts humantime durations like `30s`, `5m`, `1h30m`, `1d`.
    #[clap(long, value_parser = humantime::parse_duration)]
    pub timeout: Option<Duration>,
    /// Working directory for the child process.
    ///
    /// If unset, uses the client's cwd when running on the same
    /// machine as the daemon, or the daemon's configured default
    /// otherwise.
    #[clap(long, short = 'w')]
    pub workdir: Option<PathBuf>,
    /// Set an environment variable (`KEY=VALUE`). Repeatable.
    ///
    /// Applied on top of the inherited environment (unless
    /// `--no-inherit-env` is set) and any variables loaded from
    /// `--env-file`. Later `-e` values override earlier ones.
    #[clap(long, short = 'e')]
    pub env: Vec<String>,
    /// Load environment variables from a dotenv-style file.
    /// Lines of the form `KEY=VALUE`; `#` comments and blank lines
    /// are ignored. Repeatable.
    ///
    /// Values from `--env-file` are overridden by `-e` flags and,
    /// when inheritance is enabled, by inherited values.
    #[clap(long)]
    pub env_file: Vec<PathBuf>,
    /// Don't inherit the environment from the client/daemon when
    /// constructing the child process's environment. Only the
    /// variables explicitly set via `-e` and `--env-file` are
    /// passed through.
    #[clap(long)]
    pub no_inherit_env: bool,
    /// Allocate a pseudo-TTY for the child process.
    ///
    /// Defaults to off for `cairn exec`, on for `cairn run`. Pair
    /// with `--interactive` (typical) or use alone for programs
    /// that want a TTY but no stdin.
    #[clap(long, short = 't', overrides_with = "no_tty")]
    pub tty: bool,
    /// Negate `--tty`. Useful with `cairn run` to disable its
    /// default `-t`.
    #[clap(long, overrides_with = "tty")]
    pub no_tty: bool,
    /// Keep the client's stdin connected to the child process.
    ///
    /// Defaults to off for `cairn exec`, on for `cairn run`.
    #[clap(long, short = 'i', overrides_with = "no_interactive")]
    pub interactive: bool,
    /// Negate `--interactive`. Useful with `cairn run` to disable
    /// its default `-i`.
    #[clap(long, overrides_with = "interactive")]
    pub no_interactive: bool,
    /// Key sequence that detaches the client from the session
    /// without killing the underlying process.
    ///
    /// Format is comma-separated key tokens, e.g. `ctrl-q,ctrl-q`
    /// or `ctrl-a,d` (tmux-style). If unset, defaults to
    /// `ctrl-q,ctrl-q`. Only consulted when the session is
    /// attached (not with `--detach`).
    #[clap(long)]
    pub detach_keys: Option<String>,
    /// Forward signals received by the client to the new session's
    /// process group. Does not apply to SIGCHLD, SIGSTOP, or SIGKILL.
    ///
    /// Defaults to `true`. Ignored when `--detach` is set (there's no
    /// attached client whose signals could be proxied).
    #[clap(
        long,
        action = ArgAction::Set,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true",
        default_value_t = true,
    )]
    pub sig_proxy: bool,
    /// The command (and arguments) to execute. If omitted, uses
    /// the daemon's default shell.
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}

// Used at dispatch time once behavior is wired up. Scaffolding for now;
// see the ExecArgs doc-comment for the resolution rules.
//
// `--detach` is intentionally orthogonal: it only controls whether the
// *client* attaches now, not how the *session* is provisioned. A
// detached session still gets a TTY/stdin per the command default so a
// later `cairn attach` works interactively (mirroring `docker run -dit`).
#[allow(dead_code)]
impl ExecArgs {
    /// Resolve `--interactive`/`--no-interactive` against the
    /// command's default.
    pub fn interactive_with_default(&self, default: bool) -> bool {
        if self.no_interactive {
            false
        } else if self.interactive {
            true
        } else {
            default
        }
    }

    /// Resolve `--tty`/`--no-tty` against the command's default.
    pub fn tty_with_default(&self, default: bool) -> bool {
        if self.no_tty {
            false
        } else if self.tty {
            true
        } else {
            default
        }
    }
}

/// When to emit color in client output.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorChoice {
    /// Use color when stdout is a TTY and `NO_COLOR` is unset.
    #[default]
    Auto,
    /// Always emit color, even when piping.
    Always,
    /// Disable color output.
    Never,
}

/// A name or uuid identifying a session.
/// We use a string here because there would be no value
/// in parsing the UUID because a failure would just fall back
/// to a name which accepts anything.
/// The daemon will check if the string is a known session name first,
/// then attempt to parse it as a UUID if no name match is found,
/// and return an error if neither matches.
type SessionId = String;

/// A signal to send to a session's child process.
///
/// Names are carried symbolically across the wire so the daemon can
/// resolve them against its own libc — this avoids the Linux/BSD
/// numbering divergence (e.g. `SIGUSR1` is 10 on Linux but 30 on
/// macOS/BSD). The numeric variant is an escape hatch meaning "this
/// exact OS-specific number" — same semantics as `kill -10` at the
/// shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Named(SignalName),
    Number(u8),
}

/// POSIX-portable signal names. The daemon resolves each to its local
/// libc constant. Linux-only (STKFLT, PWR) and BSD-only (INFO) names
/// are intentionally omitted; users wanting them can pass the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString, strum::Display)]
#[strum(ascii_case_insensitive)]
pub enum SignalName {
    #[strum(to_string = "HUP", serialize = "SIGHUP")]
    Hup,
    #[strum(to_string = "INT", serialize = "SIGINT")]
    Int,
    #[strum(to_string = "QUIT", serialize = "SIGQUIT")]
    Quit,
    #[strum(to_string = "ILL", serialize = "SIGILL")]
    Ill,
    #[strum(to_string = "TRAP", serialize = "SIGTRAP")]
    Trap,
    #[strum(to_string = "ABRT", serialize = "SIGABRT")]
    Abrt,
    #[strum(to_string = "BUS", serialize = "SIGBUS")]
    Bus,
    #[strum(to_string = "FPE", serialize = "SIGFPE")]
    Fpe,
    #[strum(to_string = "KILL", serialize = "SIGKILL")]
    Kill,
    #[strum(to_string = "USR1", serialize = "SIGUSR1")]
    Usr1,
    #[strum(to_string = "SEGV", serialize = "SIGSEGV")]
    Segv,
    #[strum(to_string = "USR2", serialize = "SIGUSR2")]
    Usr2,
    #[strum(to_string = "PIPE", serialize = "SIGPIPE")]
    Pipe,
    #[strum(to_string = "ALRM", serialize = "SIGALRM")]
    Alrm,
    #[strum(to_string = "TERM", serialize = "SIGTERM")]
    Term,
    #[strum(to_string = "CHLD", serialize = "SIGCHLD")]
    Chld,
    #[strum(to_string = "CONT", serialize = "SIGCONT")]
    Cont,
    #[strum(to_string = "STOP", serialize = "SIGSTOP")]
    Stop,
    #[strum(to_string = "TSTP", serialize = "SIGTSTP")]
    Tstp,
    #[strum(to_string = "TTIN", serialize = "SIGTTIN")]
    Ttin,
    #[strum(to_string = "TTOU", serialize = "SIGTTOU")]
    Ttou,
    #[strum(to_string = "URG", serialize = "SIGURG")]
    Urg,
    #[strum(to_string = "XCPU", serialize = "SIGXCPU")]
    Xcpu,
    #[strum(to_string = "XFSZ", serialize = "SIGXFSZ")]
    Xfsz,
    #[strum(to_string = "VTALRM", serialize = "SIGVTALRM")]
    Vtalrm,
    #[strum(to_string = "PROF", serialize = "SIGPROF")]
    Prof,
    #[strum(to_string = "WINCH", serialize = "SIGWINCH")]
    Winch,
    #[strum(to_string = "IO", serialize = "SIGIO")]
    Io,
    #[strum(to_string = "SYS", serialize = "SIGSYS")]
    Sys,
}

impl Default for Signal {
    fn default() -> Self {
        Self::Named(SignalName::Term)
    }
}

impl FromStr for Signal {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err("signal cannot be empty".into());
        }

        // Numeric escape hatch first — parse as u16 so we can give a
        // range error instead of a generic "invalid digit" for 256+.
        if trimmed.chars().all(|c| c.is_ascii_digit()) {
            let n: u16 = trimmed
                .parse()
                .map_err(|_| format!("invalid signal number: {s}"))?;
            return match n {
                1..=255 => Ok(Signal::Number(n as u8)),
                _ => Err(format!("signal number {n} out of range 1..=255")),
            };
        }

        SignalName::from_str(trimmed)
            .map(Signal::Named)
            .map_err(|_| format!("unknown signal: {s}"))
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Signal::Named(n) => write!(f, "{n}"),
            Signal::Number(n) => write!(f, "{n}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, Signal, SignalName};
    use clap::CommandFactory;

    #[test]
    fn verify_cli() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_bare_name() {
        assert_eq!(
            "INT".parse::<Signal>().unwrap(),
            Signal::Named(SignalName::Int)
        );
        assert_eq!(
            "TERM".parse::<Signal>().unwrap(),
            Signal::Named(SignalName::Term)
        );
    }

    #[test]
    fn parses_sig_prefix_and_case_insensitive() {
        let expected = Signal::Named(SignalName::Int);
        assert_eq!("SIGINT".parse::<Signal>().unwrap(), expected);
        assert_eq!("sigint".parse::<Signal>().unwrap(), expected);
        assert_eq!(
            "SigKill".parse::<Signal>().unwrap(),
            Signal::Named(SignalName::Kill)
        );
    }

    #[test]
    fn parses_numeric_as_exact_os_specific_number() {
        // Numeric form is the escape hatch: forwarded as-is, not mapped
        // back to a named variant even if the number would match one.
        assert_eq!("1".parse::<Signal>().unwrap(), Signal::Number(1));
        assert_eq!("9".parse::<Signal>().unwrap(), Signal::Number(9));
        assert_eq!("255".parse::<Signal>().unwrap(), Signal::Number(255));
    }

    #[test]
    fn rejects_numeric_out_of_range() {
        assert!("0".parse::<Signal>().is_err());
        assert!("256".parse::<Signal>().is_err());
        assert!("99999".parse::<Signal>().is_err());
    }

    #[test]
    fn rejects_unknown_name() {
        assert!("BOGUS".parse::<Signal>().is_err());
        assert!("SIGBOGUS".parse::<Signal>().is_err());
        assert!("".parse::<Signal>().is_err());
    }

    #[test]
    fn display_renders_canonical_name_for_named() {
        assert_eq!(Signal::Named(SignalName::Int).to_string(), "INT");
        assert_eq!(Signal::Named(SignalName::Term).to_string(), "TERM");
        assert_eq!(Signal::default().to_string(), "TERM");
    }

    #[test]
    fn display_renders_decimal_for_number() {
        assert_eq!(Signal::Number(2).to_string(), "2");
        assert_eq!(Signal::Number(40).to_string(), "40");
        assert_eq!(Signal::Number(255).to_string(), "255");
    }
}
