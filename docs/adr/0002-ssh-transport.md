# ADR-0002: SSH transport, host-key pinning and PTY sessions

- **Status:** Accepted (M1)
- **Scope:** S-012, S-013. Also governs the exec channel that S-031 and S-032 use.

## Context

Selai must authenticate to SSH servers with passwords and private keys, verify
the server's host key before any credential leaves the machine, and run an
interactive PTY. The spec forbids any "accept any key" handler and any
host-key bypass. AGENTS.md requires strict pinning after first trust.

## Decision

1. **Library:** `russh` 0.64.x (async, Tokio, pure Rust crypto backend). Its
   `client::Handler::check_server_key` is called during key exchange, before
   user authentication. Rejecting the key there means no credential is sent.
2. **Two-phase trust.**
   - *Probe:* `probe_host_key` connects, completes key exchange, records the
     server key, and disconnects. It never calls an authentication method.
   - *Connect:* `connect_authenticated` accepts the server key only if it equals
     a pinned key for that host (same algorithm and same public-key bytes).
     Anything else is rejected during key exchange, and the error carries the
     observed key so the UI can show it.
3. **Pinning is in our database, not `known_hosts`.** Pins live in
   `known_host_keys` (one active row per host and algorithm). Rotation retires
   the old row (`replaced_at_ms`) and inserts the new one in one transaction, so
   the history stays. We do not read or write `~/.ssh/known_hosts`.
4. **Trust must match what the user saw.** The UI does not send a key to pin.
   The probe result is cached in Rust for the host for five minutes. `host_trust`
   accepts only a fingerprint equal to a key in that cache. A webview cannot
   pin an arbitrary key.
5. **Certificates are rejected in v1.** A server presenting an OpenSSH host
   certificate fails verification with `SSH_HOSTKEY_UNKNOWN`. Certificate trust
   is not in scope.
6. **Authentication:** password, or private-key file with optional passphrase
   (`russh::keys::load_secret_key`). If password authentication is refused and
   the server offers keyboard-interactive, retry with the saved password for
   a single hidden `Password:` prompt. Empty informational rounds are bounded;
   OTP, password changes, multiple prompts and repeated password requests are
   rejected by this automatic fallback. Full interactive authentication and
   OS-provided methods use the system OpenSSH backend described in
   [SSH authentication](../ssh-authentication.md).
   Failure returns `SSH_AUTH_FAILED` and does not reveal which method was rejected.
7. **Timeouts:** the connect timeout (1..120 s, default 15) covers TCP plus key
   exchange plus authentication. Keepalive is set from the host profile.
8. **PTY:** `pty-req` with `xterm-256color` (or the host's terminal type), then
   `shell`. Output is forwarded as raw bytes. Resize sends `window-change`.
   Each terminal session has its own `session_id` bound to one `host_id`, so a
   reconnect never reuses another host's session.
9. **Exec channels** (no PTY) are reserved for the agent (S-031 onward). Manual
   terminal and AI execution are separate code paths.

## Implementation notes (M1)

- OpenSSH sends `EOF` before `exit-status`. The shell stream therefore ends on
  `Close`, not `EOF`, so the exit status is not lost.
- RSA private keys (2048-8192 bits) authenticate through a custom OpenSSL signer
  using `rsa-sha2-512` or `rsa-sha2-256`. This avoids RustCrypto RSA private
  operations affected by `RUSTSEC-2023-0071`; decoding and public verification
  remain in russh. The same validation runs at import and connection time.
  Servers without `server-sig-algs` are tried with SHA-512 then SHA-256; SHA-1
  is never used for user authentication. DSA keys remain unsupported.
  Ed25519 and ECDSA keys retain the native russh authentication path.
- `probe_host_key` makes no authentication attempt. Tests check the server log
  for `Accepted` or `Failed` lines to confirm this.

## Consequences

- Host-key failures are detected before any password is sent. The integration
  tests check this with a wrong pinned key and a correct password, and expect
  `SSH_HOSTKEY_CHANGED` with no auth error.
- The probe is an extra TCP connection per trust decision. This is acceptable
  because trust happens once per host.
- `russh` takes passwords as `String`, so one copy of the password exists in
  memory outside our zeroizing wrapper for the length of the auth call. This is
  a known limit and is documented in the security notes.
- `russh` default algorithm set is used. Algorithm compatibility is tested
  against OpenSSH 9.6 only in M1.

## Fallback

If `russh` cannot complete key exchange with a supported server, the next
candidate is `ssh2` (libssh2 binding). Switching requires a new ADR, because the
verification hook differs.

## Impact on other agents

- Agent D (agent/policy) uses `SshSession::exec` only after policy approval and
  audit pre-record. Do not call exec from UI code.
- Host-key rotation UI must go through the trust flow above.


## Expanded user authentication

Profiles can select system OpenSSH for automatic/MFA, keyboard-interactive,
agent, user certificate, FIDO, PKCS#11, GSSAPI or host-based authentication.
The embedded backend remains available for existing password/vault-key profiles.
Both backends share the Session interface for shells, exec, SFTP and forwarding.
Native authentication uses an in-memory askpass broker with host-bound,
expiring capabilities and response buffers that are cleared on drop. An
isolated per-session known_hosts file contains only Selai's pins; native
connections perform strict verification after a credential-free preflight probe.
No native connection check executes a remote command. See the linked support
matrix for platform requirements, separate-connection behavior and live-test
limits. Migration 0010 adds an expanded authentication column without rebuilding
hosts or changing existing host IDs, foreign keys, history or pins.
