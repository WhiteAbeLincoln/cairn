# WebTransport quickstart

## Without certificates (self-signed, same machine)

**Terminal 1 -- daemon:**
```bash
cargo run -p cairn-daemon -- --listen unix --listen wt://127.0.0.1:4433 --auth none
```

The daemon generates a self-signed ECDSA P-256 cert, writes the hash to `$TMPDIR/cairn/cert-hash`, and logs it. Both UDS and WT listeners start.

**Terminal 2 -- client over WT:**
```bash
# The CLI auto-loads the cert hash from $TMPDIR/cairn/cert-hash for loopback
cargo run -p cairn -- --daemon wt://127.0.0.1:4433 version

# Create a session and list it
cargo run -p cairn -- --daemon wt://127.0.0.1:4433 run -- echo hello
cargo run -p cairn -- --daemon wt://127.0.0.1:4433 list
```

Compare with UDS (should show the same sessions):
```bash
cargo run -p cairn -- list
```

## With Tailscale certificates (remote, cross-machine)

On the machine running the daemon:
```bash
# Get your Tailscale hostname
tailscale status | head -1

# Generate a cert for your Tailscale hostname
tailscale cert --cert-path /tmp/cairn-ts.crt --key-path /tmp/cairn-ts.key your-hostname.ts.net

# Start daemon on the Tailscale IP
cargo run -p cairn-daemon -- \
  --listen unix \
  --listen wt://0.0.0.0:4433 \
  --wt-cert /tmp/cairn-ts.crt \
  --wt-key /tmp/cairn-ts.key \
  --auth tailscale
```

From another Tailscale machine:
```bash
# Tailscale certs are CA-signed, so no --cert-hash needed
cargo run -p cairn -- --daemon wt://your-hostname.ts.net:4433 version
cargo run -p cairn -- --daemon wt://your-hostname.ts.net:4433 whoami
# ^ should show your Tailscale login name
```

## Quick smoke test (both transports, one terminal)

```bash
# Start daemon in background with both listeners
cargo run -p cairn-daemon -- --listen unix --listen wt://127.0.0.1:4433 --auth none &
sleep 1

# UDS
cargo run -p cairn -- version
cargo run -p cairn -- whoami

# WT (auto-pins cert hash for loopback)
cargo run -p cairn -- --daemon wt://127.0.0.1:4433 version
cargo run -p cairn -- --daemon wt://127.0.0.1:4433 whoami

# Kill daemon
kill %1
```
