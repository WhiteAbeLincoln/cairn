# WebTransport quickstart

WebTransport endpoints use the `https://` URL scheme — the one defined by
the W3C WebTransport spec. (Earlier revisions of cairn used a `wt://`
alias, but that scheme is not registered with IANA and has been removed.)

## Without certificates (self-signed, same machine)

**Terminal 1 -- daemon:**
```bash
cargo run -p cairn-daemon -- --listen unix --listen https://127.0.0.1:4433 --auth none
```

The daemon generates a self-signed ECDSA P-256 cert, writes the hash to `$TMPDIR/cairn/cert-hash`, and logs it. Both UDS and WT listeners start.

**Terminal 2 -- client over WT:**
```bash
# The CLI auto-loads the cert hash from $TMPDIR/cairn/cert-hash for loopback
cargo run -p cairn -- --daemon https://127.0.0.1:4433 version

# Create a session and list it
cargo run -p cairn -- --daemon https://127.0.0.1:4433 run -- echo hello
cargo run -p cairn -- --daemon https://127.0.0.1:4433 list
```

Compare with UDS (should show the same sessions):
```bash
cargo run -p cairn -- list
```

## With Tailscale certificates (remote, cross-machine)

Prerequisites on the daemon host (one-time):

```bash
# Grant the user that will run cairn-daemon access to the Tailscale LocalAPI.
# Without this, the daemon's calls to /localapi/v0/whois come back 403 and
# every incoming WebTransport connection is rejected with "access denied by
# tailscaled". tailscaled only grants LocalAPI read permission to uid 0 or
# the configured operator.
sudo tailscale set --operator=$(whoami)
```

> macOS: the GUI Tailscale app from the App Store uses a different LocalAPI
> path (a localhost TCP listener with a `sameuserproof` token under
> `/Library/Tailscale/`) and does not need `--operator`. The daemon detects
> which install you have at startup. If you installed `tailscaled` via
> Homebrew on macOS, you still need `--operator`.

On the machine running the daemon:
```bash
# Get your Tailscale hostname
tailscale status | head -1

# Generate a cert for your Tailscale hostname
tailscale cert --cert-path /tmp/cairn-ts.crt --key-path /tmp/cairn-ts.key your-hostname.ts.net

# Start daemon on the Tailscale IP
cargo run -p cairn-daemon -- \
  --listen unix \
  --listen https://0.0.0.0:4433 \
  --wt-cert /tmp/cairn-ts.crt \
  --wt-key /tmp/cairn-ts.key \
  --auth tailscale
```

From another Tailscale machine:
```bash
# Tailscale certs are CA-signed, so no --cert-hash needed
cargo run -p cairn -- --daemon https://your-hostname.ts.net:4433 version
cargo run -p cairn -- --daemon https://your-hostname.ts.net:4433 whoami
# ^ should show your Tailscale login name
```

## Quick smoke test (both transports, one terminal)

```bash
# Start daemon in background with both listeners
cargo run -p cairn-daemon -- --listen unix --listen https://127.0.0.1:4433 --auth none &
sleep 1

# UDS
cargo run -p cairn -- version
cargo run -p cairn -- whoami

# WT (auto-pins cert hash for loopback)
cargo run -p cairn -- --daemon https://127.0.0.1:4433 version
cargo run -p cairn -- --daemon https://127.0.0.1:4433 whoami

# Kill daemon
kill %1
```
