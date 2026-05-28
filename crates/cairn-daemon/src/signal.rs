//! Translate the protocol `signal` to a `nix::sys::signal::Signal`. Named
//! signals resolve against THIS host's libc (so SIGUSR1 etc. are correct on
//! both Linux and BSD); the numbered variant is an as-is escape hatch.

use nix::sys::signal::Signal as NixSignal;

use cairn_protocol::cairn::daemon::types::{Signal, SignalName};

use crate::error::DaemonError;

pub fn to_nix(sig: &Signal) -> Result<NixSignal, DaemonError> {
    match sig {
        Signal::Numbered(0) => Err(DaemonError::InvalidSignal),
        Signal::Numbered(n) => {
            NixSignal::try_from(i32::from(*n)).map_err(|_| DaemonError::InvalidSignal)
        }
        Signal::Named(name) => Ok(named_to_nix(*name)),
    }
}

fn named_to_nix(name: SignalName) -> NixSignal {
    match name {
        SignalName::Hup => NixSignal::SIGHUP,
        SignalName::Int => NixSignal::SIGINT,
        SignalName::Quit => NixSignal::SIGQUIT,
        SignalName::Ill => NixSignal::SIGILL,
        SignalName::Trap => NixSignal::SIGTRAP,
        SignalName::Abrt => NixSignal::SIGABRT,
        SignalName::Bus => NixSignal::SIGBUS,
        SignalName::Fpe => NixSignal::SIGFPE,
        SignalName::Kill => NixSignal::SIGKILL,
        SignalName::Usr1 => NixSignal::SIGUSR1,
        SignalName::Segv => NixSignal::SIGSEGV,
        SignalName::Usr2 => NixSignal::SIGUSR2,
        SignalName::Pipe => NixSignal::SIGPIPE,
        SignalName::Alrm => NixSignal::SIGALRM,
        SignalName::Term => NixSignal::SIGTERM,
        SignalName::Chld => NixSignal::SIGCHLD,
        SignalName::Cont => NixSignal::SIGCONT,
        SignalName::Stop => NixSignal::SIGSTOP,
        SignalName::Tstp => NixSignal::SIGTSTP,
        SignalName::Ttin => NixSignal::SIGTTIN,
        SignalName::Ttou => NixSignal::SIGTTOU,
        SignalName::Urg => NixSignal::SIGURG,
        SignalName::Xcpu => NixSignal::SIGXCPU,
        SignalName::Xfsz => NixSignal::SIGXFSZ,
        SignalName::Vtalrm => NixSignal::SIGVTALRM,
        SignalName::Prof => NixSignal::SIGPROF,
        SignalName::Winch => NixSignal::SIGWINCH,
        SignalName::Io => NixSignal::SIGIO,
        SignalName::Sys => NixSignal::SIGSYS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_protocol::cairn::daemon::types::{Signal, SignalName};

    #[test]
    fn named_term_resolves_to_libc_sigterm() {
        assert_eq!(to_nix(&Signal::Named(SignalName::Term)).unwrap(), NixSignal::SIGTERM);
        assert_eq!(to_nix(&Signal::Named(SignalName::Kill)).unwrap(), NixSignal::SIGKILL);
        assert_eq!(to_nix(&Signal::Named(SignalName::Int)).unwrap(), NixSignal::SIGINT);
    }

    #[test]
    fn numbered_passes_through() {
        assert_eq!(to_nix(&Signal::Numbered(9)).unwrap(), NixSignal::SIGKILL);
    }

    #[test]
    fn numbered_zero_is_invalid() {
        assert!(to_nix(&Signal::Numbered(0)).is_err());
    }

    #[test]
    fn numbered_out_of_range_is_invalid() {
        assert!(to_nix(&Signal::Numbered(255)).is_err());
    }
}
