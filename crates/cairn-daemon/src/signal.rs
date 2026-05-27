//! Translate the protocol `signal` to a local libc signal number. Named
//! signals resolve against THIS host's libc (so SIGUSR1 etc. are correct on
//! both Linux and BSD); the numbered variant is an as-is escape hatch.

use cairn_protocol::cairn::daemon::types::{Signal, SignalName};

use crate::error::DaemonError;

pub fn to_libc(sig: &Signal) -> Result<i32, DaemonError> {
    match sig {
        Signal::Numbered(0) => Err(DaemonError::InvalidSignal),
        Signal::Numbered(n) => Ok(i32::from(*n)),
        Signal::Named(name) => Ok(named_to_libc(*name)),
    }
}

fn named_to_libc(name: SignalName) -> i32 {
    match name {
        SignalName::Hup => libc::SIGHUP,
        SignalName::Int => libc::SIGINT,
        SignalName::Quit => libc::SIGQUIT,
        SignalName::Ill => libc::SIGILL,
        SignalName::Trap => libc::SIGTRAP,
        SignalName::Abrt => libc::SIGABRT,
        SignalName::Bus => libc::SIGBUS,
        SignalName::Fpe => libc::SIGFPE,
        SignalName::Kill => libc::SIGKILL,
        SignalName::Usr1 => libc::SIGUSR1,
        SignalName::Segv => libc::SIGSEGV,
        SignalName::Usr2 => libc::SIGUSR2,
        SignalName::Pipe => libc::SIGPIPE,
        SignalName::Alrm => libc::SIGALRM,
        SignalName::Term => libc::SIGTERM,
        SignalName::Chld => libc::SIGCHLD,
        SignalName::Cont => libc::SIGCONT,
        SignalName::Stop => libc::SIGSTOP,
        SignalName::Tstp => libc::SIGTSTP,
        SignalName::Ttin => libc::SIGTTIN,
        SignalName::Ttou => libc::SIGTTOU,
        SignalName::Urg => libc::SIGURG,
        SignalName::Xcpu => libc::SIGXCPU,
        SignalName::Xfsz => libc::SIGXFSZ,
        SignalName::Vtalrm => libc::SIGVTALRM,
        SignalName::Prof => libc::SIGPROF,
        SignalName::Winch => libc::SIGWINCH,
        SignalName::Io => libc::SIGIO,
        SignalName::Sys => libc::SIGSYS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_protocol::cairn::daemon::types::{Signal, SignalName};

    #[test]
    fn named_term_resolves_to_libc_sigterm() {
        assert_eq!(to_libc(&Signal::Named(SignalName::Term)).unwrap(), libc::SIGTERM);
        assert_eq!(to_libc(&Signal::Named(SignalName::Kill)).unwrap(), libc::SIGKILL);
        assert_eq!(to_libc(&Signal::Named(SignalName::Int)).unwrap(), libc::SIGINT);
    }

    #[test]
    fn numbered_passes_through() {
        assert_eq!(to_libc(&Signal::Numbered(9)).unwrap(), 9);
    }

    #[test]
    fn numbered_zero_is_invalid() {
        assert!(to_libc(&Signal::Numbered(0)).is_err());
    }
}
