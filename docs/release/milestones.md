# Milestone status

Status values: **done** (test evidence recorded below), **partial** (missing
items listed), **not started**. Evidence is the command and result at the time
of the last update.

## M0 — Foundation

| Task | Status | Gaps |
|---|---|---|
| S-001 Scaffold monorepo and desktop UI | partial | Windows and macOS builds are in CI but were not run here. |
| S-002 CI, ADRs, security baseline | partial | ADR-0004 and ADR-0005 are not written (see `docs/adr/README.md`). Gitleaks has not been run locally. |
| S-003 SQLite repository and migration | partial | Pagination is implemented for hosts only. Other repositories are not written yet. |

## M1 — Working SSH client (`v1.0.0-alpha.1` candidate)

| Task | Status | Evidence | Gaps |
|---|---|---|---|
| S-010 Host CRUD, labels, groups | done | `selai-storage/tests/hosts.rs` (18), domain field tests, profile-management service tests; frontend checks cover group creation, selection, rename/delete and local search; browser fixture verifies collapsible groups and the profile editor | Latest group UI runtime check uses a mock IPC backend; native Windows/macOS UI remains unverified locally. |
| S-011 Native credential store | done on Linux | `selai-secrets/tests/keyring_backend.rs` against a live Secret Service; in-app check: secret stored under `service=selai, username=ssh/<id>/password`; database has zero occurrences of the password | macOS Keychain and Windows Credential Manager tests run in CI but were not run here. Locked-store behaviour is tested with an in-memory store only. |
| S-012 SSH transport, host-key verification | done on Linux | 19 transport integration tests against OpenSSH 9.2p1 on 2026-10-18 (`selai-ssh/tests/ssh_integration.rs`): host trust, password/Ed25519 login, RSA-SHA2-256/512 login, encrypted RSA PEM from memory, incorrect passphrases, SHA-1 refusal, terminal/exec and timeouts; 10 unit tests and 5 sudo integration tests also pass | ECDSA login is not tested. Latest RSA runtime evidence is Linux only; native Windows and macOS login remain unverified locally. |
| S-013 Interactive PTY | partial | In-app: prompt, command output, exit status 0 reported; restart and reconnect works; service test with five concurrent sessions bound to their own hosts; resize reaches the PTY | `vim` and `top` are not tested. Backpressure is not implemented: the IPC queue is unbounded. Terminal scrollback is bounded by xterm (5000 lines). |

RSA service coverage (2026-10-18): `cargo test -p selai-desktop --test service_e2e
--locked rsa -- --test-threads=1` passes two tests on Linux. The tests import
OpenSSH and encrypted PEM RSA keys, select each managed key in a profile, trust
the server fingerprint, execute through a terminal, and reject an incorrect
import passphrase without saving a key. Transport coverage uses
`cargo test -p selai-ssh --locked` (34 tests pass).

### M1 milestone gate

Workbench update (2026-10-18): terminal exit now closes its tab and releases the
SSH worker without waiting for a channel-close message. Profile password reveal
is explicit, clears on vault flushing, and is tested against locked storage and
the encrypted vault lifecycle. Deleting a group keeps profiles and credentials;
its transaction also invalidates stale host-editor revisions. Tests cover
simultaneous terminal exits and discard late password-reveal responses.

The gate is: configure `production-web`, trust its fingerprint, connect by
password and by key, use the terminal, restart Selai, and use it again.

Checked on Linux against a real `sshd` (fixture in `selai-testkit`):

- Password host created in the UI, key probed, fingerprint trusted, shell opened,
  command run, exit status reported (`exit 0`).
- Application restarted with the same data directory and keychain. Connect worked
  with no re-trust and no password entry. Shell ran `restarted-42`.
- Encrypted-key login with the passphrase from the OS store: service-level test.

Not yet checked: the gate on macOS and Windows, and a real host other than the
test fixture.

### Test evidence (last run)

- `cargo fmt --all -- --check`: clean
- `cargo clippy --workspace --all-targets -- -D warnings`: clean
- `scripts/ci/with-keyring.sh cargo test --workspace` (as root, so sshd can run):
  domain 23, secrets 5 (4 contract + 1 keyring), selai-ssh 13, storage 22 unit
  + 15 integration, desktop unit 2, desktop end-to-end 10. All passed.
- Frontend: typecheck, lint, 11 tests, build: pass.
- `cargo deny check advisories licenses bans sources`: pass. One ignored
  advisory, RUSTSEC-2023-0071, with the reason in `deny.toml`.

## M2 — Universal AI provider configuration

| Task | Status | Evidence | Gaps |
|---|---|---|---|
| S-020 Provider CRUD, secure keys | done | `selai-storage/tests/providers.rs` (6), `selai-desktop/tests/provider_e2e.rs` (10): key stored in the OS store, never in the database, responses, or request bodies; locked store fails closed; delete removes the key. In-app: provider created through the UI, key field write-only | Edit and delete were checked in tests; not every edit path was clicked in the app. |
| S-021 OpenAI Responses and Chat adapters | done against mock servers | `selai-llm/tests/adapters.rs`: text streams, fragmented tool arguments, malformed JSON never executes, cut streams fail, redirects not followed, 429 retried only before output, 401 and tool rejections classified, cancellation | Not verified against the live OpenAI API (no key supplied). |
| S-022 Anthropic Messages adapter | done against mock servers | `adapters.rs`: `tool_use` with partial JSON, parallel calls, `tool_result` ordering (merged into one user message), error events | Not verified against the live Anthropic API. |
| S-023 Compatible templates and probes | partial | `probe.rs` (6): verified only after a full round trip; text-only and refusing models reported as not verified. Templates in `selai-llm/src/templates.rs`, with notes | GLM and Kimi base URLs come from third-party docs, not the vendor's. No live smoke test for any compatible server. Model IDs are never hardcoded. |

### M2 milestone gate

The gate is: a model produces a genuine tool call and receives the tool result
through the selected provider, with no SSH involved.

Checked in the app: a provider pointed at a local OpenAI-compatible mock
(`/tmp/mock_provider.py`, a local script, not part of the repo) was created
through the UI. "Test tool calling" showed *tool calling verified for this
model*. The round trip was real: a tool call, the tool result sent back, and a
final answer.

**Not verified against any live provider.** The `live` tests in
`crates/selai-llm/tests/live.rs` are ignored until someone supplies keys (see
CONTRIBUTING.md). Until they pass, every provider is reported as not verified
against a live endpoint.

### Test evidence (last run)

- `cargo test --workspace` through the keyring harness: domain 29, secrets 5,
  selai-llm 9 unit + 21 adapter + 6 probe, storage 22 + 15 + 6, SSH 13, desktop
  unit 2, service e2e 10, provider e2e 10. All passed. Ignored: 3 live provider
  tests.
- Frontend: 14 tests, typecheck, lint, build pass.
- `cargo deny check advisories licenses bans sources`: pass. Added
  CDLA-Permissive-2.0 for the Mozilla CA bundle (reason in `deny.toml`).

## M3 — Scoped AI agent (`v1.0.0-alpha.2` candidate)

| Task | Status | Evidence | Gaps |
|---|---|---|---|
| S-030 Mention picker and host binding | done | Chip stores the host id and a label snapshot; the model never chooses the host (`selai-agent/src/tools.rs` has no host argument). Picker: arrow keys, Enter and click are tested (`AgentPanel.test.tsx`); Escape is not. Archived host refused (`agent_e2e.rs`). The host is bound by id, so a rename cannot retarget a running task by design, but no test covers a rename. | The picker accepts one host per request (multi-target refused). Free text with an `@name` that was not picked is sent as plain text and binds no host. |
| S-031 Policy and approval | done | `selai-policy` (12): diagnostics map to fixed commands (`df -hP`, `uptime`, ...), free-form commands always need approval, out-of-scope host denied. Approval tickets are bound to task, host and command digest, and expire. Runtime tests (`selai-agent/tests/runtime.rs`, 14): tampered command, changed host and expired ticket all refuse. `agent_e2e.rs`: approve runs the exact command, deny never runs it, cancel while waiting never runs it. | There are no policy roles; there is one policy. The UI disables Approve once an approval expires only when the card re-renders; the database refuses the decision either way. |
| S-032 Audit before execution | done | `selai-audit` (11): redaction and sha256 hash chain. `selai-storage/tests/agent.rs` (6): execution row and audit event written in one transaction before any SSH I/O. `agent_e2e.rs`: five diagnostics each recorded, chain verifies, history survives a restart. Model payloads checked for the SSH password and the provider key. | Blob encryption of full output is deferred to S-040. Only bounded, redacted previews are stored. |
| S-033 Agent runtime and SSH bridge | done on Linux | `selai-ssh` exec (in `ssh_integration.rs`, 17 SSH tests in total), output capped per stream, cancellation. `agent_e2e.rs` (7) runs against real OpenSSH and a mock provider that must pass tool calling first. | Checkpoints now written (see M4). No automatic reconnect. No trust grants. Only Linux was run. |

### M3 milestone gate

The gate is: in the app, type `@production-web check disk usage`, see the
result from the real host, approve a free-form command and see its result, and
find the command in that host's history after a restart.

Checked by the desktop end-to-end suite (`apps/desktop/src-tauri/tests/agent_e2e.rs`)
against a real `sshd` and a mock provider: the disk check runs on the bound host
and is recorded; the approved command runs only after approval; the denied and
cancelled commands never run; history and audit survive a restart.

**Not checked in a real window in this session.** The React panel is covered by
component tests with mocked IPC (`AgentPanel.test.tsx`, 7 tests; transcript reducer, 6), not by driving
the running application. **Not verified against any live provider.** The
`live` tests stay ignored until keys are supplied. macOS and Windows were not run.

Bug found and fixed while building M3: an approval click that arrived before the
task began waiting was dropped, and the approval then expired. The registry now
keeps an early decision for the next wait (`agent_service.rs`, tests). Found
during review, not by a failing test.

### Test evidence (last run)

- `cargo clippy --workspace --all-targets --locked -- -D warnings`: clean.
- `scripts/ci/with-keyring.sh cargo test --workspace --locked` (as root, so sshd
  runs): all passed. Per target: domain 29, selai-llm 9 unit + 21 adapter + 6
  probe, policy 12, audit 11, agent runtime 14, storage 22 unit + 15 hosts + 6
  providers + 6 agent, SSH 17, secrets 1 keyring + 4 contract, desktop unit 5,
  desktop end-to-end: service 10, provider 10, agent 7. Ignored: 3 live provider
  tests.
- Frontend: typecheck, lint, 27 tests, `vite build`: pass.
- `cargo deny check`: advisories, bans, licenses, sources ok.

## M4 — History, privacy and hardening (`v1.0.0-beta.1` candidate)

| Task | Status | Evidence | Gaps |
|---|---|---|---|
| S-040 Per-host history UI and export | done (not run in a real window) | `apps/desktop/src/tabs/HistoryTab.test.tsx` (5); `apps/desktop/src-tauri/tests/history_e2e.rs` (6): filters, pagination, timeline, rename and archive keep history, export without the secret, purge, preferences. `selai-storage/tests/history.rs` (8). | Encrypted audit blobs deferred (S-040 spec). Export is capped at 1000 rows and says so. The `cancelled` filter is not covered end to end. |
| S-041 Provider and host security regression | done, with limits | `docs/release/security-regression.md` maps each spec threat to a test. New: hostile output (`runtime.rs`, 3 tests), hostile diagnostic inputs (`selai-policy`, 3 tests), webview hardening (3 tests). | Redaction is pattern-based; limits are documented there. |
| S-042 Persistence, recovery, budgets | partial | Startup reconciliation marks running work `interrupted`, expires stale approvals, and writes an audit event (`history.rs`). Output retention: a sweep at startup (default 30 days, configurable) and a manual purge; both audited, metadata kept. History indexes in migration 0003. Task budgets in `Limits`. Provider choice remembered. | No total size cap, only age. Process kill during a task was not tested against the app. |
| S-043 Performance and cross-platform validation | partial | `apps/desktop/src-tauri/tests/perf_pty.rs` and `docs/release/performance.md`: five terminals, four streaming, echo latency and throughput on the sandbox VM. | Input responsiveness during an AI stream not measured. Keychain tested on Linux only. Memory measured on Linux only. |

## M5 — RC / GA release (`v1.0.0` candidate)

| Task | Status | Evidence | Gaps |
|---|---|---|---|
| S-050 Installer packaging and signing pipeline | partial | `.github/workflows/release.yml` builds Linux (deb, AppImage), Windows and macOS bundles. `v0.3.1` published all platform installers, checksums (`SHA256SUMS`), CycloneDX SBOMs, and build provenance. `docs/release/packaging.md`. | Installers are not signed or notarised unless optional signing secrets are configured. In-app installation is not implemented; release discovery and browser downloads are included in 0.3.2. |
| S-051 Acceptance matrix and security signoff | partial | `docs/release/acceptance-matrix.md`: every `T-*` case from the spec, with its status. | Live-provider checks skipped (no keys). Several cases partial or not implemented (T-AUDIT-002, T-SSH-005, T-AGENT-006 process kill, T-AUDIT-003 UI backpressure). Maintainer security signoff not done. |
| S-052 Tag and publish v1.0.0 | not done | — | Waiting for the operator's confirmation. Tagging and publishing are outward-facing. |

### Test evidence (last run)

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`: clean.
- `scripts/ci/with-keyring.sh cargo test --workspace --locked`: exit 0, 39 test binaries passed. New in M4: `history_e2e` 6, storage `history` 8, storage upgrade test 1, runtime 19 (3 security, 2 checkpoint), policy 15, desktop unit 5, `webview_hardening` 3, `perf_pty` ignored (run separately).
- Frontend: typecheck, lint, 32 tests, `vite build`: pass.
- `cargo deny check advisories licenses bans sources`: pass.
- `actionlint` on `.github/workflows/release.yml` and `ci.yml`: pass.

### Application run (2026-10-16, Linux, debug build)

Built with `pnpm tauri build --debug --no-bundle` and run on a virtual display
with an unlocked keyring. Target: the OpenSSH 9.6 fixture on Ubuntu 24.04. Provider:
a local OpenAI-compatible mock with tool calling. Steps and results:

- Pass: add host through the form; first contact shows the fingerprint and trust
  is explicit; provider added and tool calling verified; the API key is never
  shown again.
- Pass: `@` picker with keyboard (arrow and Enter); `df -hP` ran on the host and
  its output was shown; the task finished `completed`.
- Pass: a free-form command shows an approval card with the exact command, host
  and expiry. Approve ran it on the host. Deny did not run it and the row reads
  `denied`.
- Pass: AI history after a restart; filters; execution details; JSON export
  (3 rows, no password or key in the file); output purge (previews cleared, rows kept,
  one `output_purged` audit event); audit chain verified (36 events).
- Pass: terminal connect and a shell command.
- Pass: the SSH password and provider key appear in neither the database, nor the
  export, nor any model request body.
- Pass: the host's key changed; connection blocked with `SSH_HOSTKEY_CHANGED`.
  The key was replaced only through the explicit replace action, after the new
  fingerprint was checked with `ssh-keyscan` out of band.
- Pass: restart keeps hosts, provider verification and history.

Bugs found by this run and fixed (each has a regression test):

1. The composer kept an empty provider list until the app restarted
   (`AgentPanel.test.tsx`: picks up a provider saved elsewhere).
2. The AI history tab loaded once and showed nothing for runs that finished
   later (`HistoryTab.test.tsx`: loads again when the tab is shown).
3. A denied command never showed its outcome in the transcript
   (`runtime.rs`: a denied command is reported as finished).

Seen and not fixed: the approval card keeps the text "Approved. Waiting for the
result." after the command has finished. Cosmetic.

Not covered by this run: Stop during a slow remote command, reconnect after a
network drop, opening an export in another application, and Windows or macOS.

### UI revamp run (2026-10-16, Linux, debug build)

Same setup as above, after the UI was rebuilt (sidebar navigation, dark and light
themes, host drawer, terminal tabs with status, chat-style AI Agent, provider
cards). Steps and results:

- Pass: new host with the label `Production Web` (with a space) through "Save &
  connect". The connect stopped at the server key, showed the fingerprint in a
  dialog, and "Trust & connect" pinned it and opened the shell. No separate host
  key step.
- Pass: a failed login showed "Authentication failed" with Retry and "Edit host";
  after editing, "Save & connect" reused the failed tab.
- Pass: provider at `http://192.0.2.2:8765/v1` (non-loopback) was refused
  without the opt-in and saved with "Allow plain HTTP". One Test click ran the
  connection check and then the tool-calling check; both passed and the
  provider showed Verified.
- Pass: AI Agent: `@Produc` listed `Production Web`, Enter picked it, and the
  task ran `df -hP` on that host. A free-form command showed one approval card;
  Approve ran it and the card showed the exit code.
- Pass: mentioning a second, untrusted host from the AI Agent showed the same
  key dialog; after trusting, the task started on its own.
- Pass: History table, execution drawer, and the light theme.

The earlier cosmetic issue (approval card text after the command finished) is
fixed: the card now reads "Approved by you." with the result badge.

Not covered: Windows and macOS rendering, and screen readers.

### Chat, token usage and model discovery run (2026-10-16, Linux, debug build)

Same setup. The database upgraded from schema v3 to v4 on open, with existing data.

- Pass: History shows schema v4 data: tasks from before the upgrade read
  "Request not recorded" with unknown (—) tokens, not zero.
- Pass: provider edit: a wrong key gave `PROVIDER_UNAUTHORIZED`; the right key
  gave "Key works · 4 models available" and a model dropdown. Saved.
- Pass: plain chat without a mention: two turns, Markdown with code blocks, the
  second request carried the first exchange (input tokens 136 then 196). No task,
  no command.
- Pass: the SSH fixture was restarted with a new host key. `@Production Web check
  disk usage` showed "Server key has changed" with the old key struck through;
  after the confirmation, the task started on its own and showed one line,
  "Ran `df -hP` exit 0", then the answer.
- Pass: an approval in the chat; after Approve the command ran.
- Pass: History: 5 tasks, 2,035 input and 160 output tokens for the host; the
  task drawer shows the request, 962 / 80 / 1,042 tokens, 2 model turns and the
  command with its output.

### Sudo and local terminal run (2026-10-16, Linux, debug build)

Same setup; the fixture user may use sudo with its password. The database
upgraded from schema v4 to v5 on open.

- Pass: Hosts shows "This computer"; it opens a local login shell in a terminal
  tab (`local-42`, colors, exit status).
- Pass: `@Production Web who am I as root?` showed "Run this command as root"
  with `sudo id -u && whoami`. The card offered "Use my SSH login password"
  (selected) and "Type a password". After "Approve as root" the command ran
  through the real sudo and the reply confirmed uid 0. The password was in none
  of the model requests and not in the database files.
- Pass: `@This computer check disk usage` ran `df -hP` on the local machine.
- Automated: real sudo over sshd (right, wrong and missing password; a command
  that reads stdin cannot capture the password), runtime fakes, and desktop e2e
  for a password host, a key host (typed, then remembered), a wrong password,
  and the local target (shell, approval, local sudo).

Not covered: local terminal and local agent on Windows and macOS (PowerShell and
the macOS shell are not run here); sudo on hosts that need a TTY (`requiretty`).

## Network workspace - 0.2.0

SFTP, local/remote/dynamic TCP forwarding, and HTTP CONNECT/SOCKS5 routes are
implemented. Native transport tests exercise all three forwarding modes and
both proxy protocols against an isolated OpenSSH fixture, including host pin
refusal and authenticated relays. Service tests cover recursive SFTP copies,
byte accounting, replacement/skip behavior, cancellation cleanup, and write-only
proxy credentials. UI flows cover saved routes, tunnel forms, upload progress,
cancellation, and large-directory virtualization.

See [ADR-0007](../adr/0007-network-workspace.md) for lifetimes, limits, storage,
and upgrade behavior. Packaging alone does not prove interactive SSH operation
on every supported OS or every external proxy implementation.
