Selai is a desktop SSH client with a scoped AI agent: save hosts, open terminals,
chat with a model, and mention a host with `@` to let the agent check it. The
model never receives SSH credentials, and every command it runs is recorded.

**This is a preview release.** Automated tests cover Linux SSH/agent flows with
fixtures and mock providers. Windows packaging also checks encryption and the
vault in the release build profile. This is not complete interactive coverage
of every platform and live AI provider. Without optional signing secrets,
Windows and Linux installers are unsigned; macOS apps use ad-hoc signing
and are not notarized.

## New in 0.4.4

- **Settings page:** move AI task budgets out of History and into Settings,
  raise the configurable limits to 300 steps and 300 minutes, and provide a
  direct entry point to vault security controls.
- **Task-scoped command approvals:** after approving the exact initial command,
  an operator may allow later free-form commands in that task on the same host.
  The grant ends when the task ends or the vault locks; commands remain in
  History and the audit log.
- **Retained network sessions while locked:** established SFTP sessions and
  port-forwarding listeners stay alive while the vault is locked. Active file
  transfers and tunnel traffic may continue until Selai exits.

No vault database migration is included in this release.

## New in 0.4.3

The release workflow now builds separate Apple Silicon (`aarch64`) and Intel
(`x64`) installers and checks each app's architecture and signature before
upload. App ZIP filenames include the architecture. Releases through v0.4.0
contain only Apple Silicon packages. On an Intel Mac,
download `Selai_0.4.3_x64.dmg`; on an M1 or newer Mac, choose the `aarch64` DMG.
Ad-hoc signing still requires approval in macOS Privacy & Security settings.
Interactive testing on each Mac model remains unverified.

This release also updates `crypto-bigint` to 0.7.10 and `ctutils` to 0.4.6,
resolving [RUSTSEC-2026-0387](https://rustsec.org/advisories/RUSTSEC-2026-0387).
Publication now requires Rust and frontend dependency audits and packaging
regression tests to pass.

## New in 0.4.0

This preview adds local and WSL environments to the SSH workspace and improves
several everyday workflows.

- **Local and WSL targets:** manage local Windows and WSL hosts alongside SSH
  profiles, open terminals, and route agent commands to the selected environment.
  WSL profiles are stored with host data and can be selected from the workspace.
- **Terminal improvements:** refine local terminal lifecycle and output handling,
  with clearer local-environment identification in the host list.
- **Safer, clearer workflows:** add confirmation dialogs for destructive actions
  and polish host groups, provider settings, history, forwarding, SFTP, and SSH
  key management.
- **Provider and vault refinements:** improve provider connection handling and
  vault unlock flows, with updated documentation and interface details.

See the README for the current feature overview. As a preview, local and WSL
execution still depends on the machine's shell and WSL configuration.

## New in 0.3.3

This preview expands SSH authentication by integrating the operating system's
OpenSSH client for methods that need live user interaction or external
credentials.

- **More authentication methods:** choose automatic/multi-factor, keyboard
  interactive/OTP, SSH agent, OpenSSH user certificate, FIDO2/security key,
  PKCS#11 smart card, Kerberos/GSSAPI, or host-based authentication. Availability
  depends on the installed OpenSSH build and any required local provider, agent,
  ticket, key, or hardware.
- **Live authentication prompts:** enter OTPs, PINs, and other responses in a
  host-bound dialog. Responses are single-use and expire; they are not saved in
  profiles, logs, command arguments, or environment variables.
- **Host-key protection:** the native backend uses an isolated known-hosts file
  with strict verification after Selai's existing probe and trust flow. It does
  not change the user's OpenSSH `known_hosts` file or add host-CA trust.
- **SSH coverage:** native authentication works with terminal sessions, SFTP,
  AI command execution, and local, dynamic, and remote forwarding. Each operation
  opens its own OpenSSH connection, so an MFA-required server can prompt again.
- **Profile migration:** saved SSH profiles gain an authentication-method
  preference. Existing profiles retain their default behavior.

See [SSH authentication](../ssh-authentication.md) for supported methods,
configuration, and validation limits. Hardware tokens, PKCS#11 providers,
Kerberos domains, and host-based trust still require testing in their respective
environments.

## New in 0.3.2

This preview adds update discovery and links to the matching installer on GitHub.

- **Check for releases:** Selai checks GitHub Releases at startup and from the
  version label. When a newer numeric release is available, the app shows its
  notes and an installer for the current platform and architecture.
- **Download manually:** the update button opens the GitHub installer in your
  browser. Selai stays open, so save your work and close the app before running
  the downloaded installer. The app does not install or restart itself.
- **No vault data in update checks:** requests use the public GitHub API without
  a GitHub token. Release notes are rendered without active links, images or HTML.
  Network limits or errors can be retried from the update dialog.

See `docs/release/packaging.md` for update discovery and download behavior.

## New in 0.3.1

This preview adds theme choices, local SSH key import, prompt recall, and keeps
terminal connections available across vault locking.

- **Catppuccin themes:** choose Latte, Frappé, Macchiato, or Mocha alongside
  dark, light, and system themes. Existing terminals, the unlock screen, and
  terminal ANSI colors follow the saved palette. Palette data is included under
  its MIT license in `docs/licenses/catppuccin.txt`.
- **Import local SSH keys:** choose a private key from `~/.ssh`, including files
  without extensions, or scan that directory for supported unencrypted keys.
  Encrypted keys can be imported individually with their passphrase; unsupported
  files and keys already in the vault are skipped.
- **Recall chat prompts:** use the up and down arrows at the composer boundary to
  move through earlier prompts while preserving the current draft. A recalled
  prompt with a host that is no longer available cannot be sent until corrected.
- **Terminal standby during vault lock:** locking blocks terminal input and new
  operations, while existing SSH transports stay connected and terminal output
  remains bounded in memory. Unlocking resumes those tabs. Closing a tab or
  quitting Selai closes its connection. See `SECURITY.md` for the full behavior.
- **Clearer task context:** agent requests include the selected host label and
  connection address, and completed command output is refreshed from the saved
  task timeline.
- **Workspace polish:** shared themed pickers and refinements to History,
  forwarding, SFTP, proxies, providers, SSH profiles, and chat.

No vault database migration is included in this release.

## New in 0.3.0

This minor release adds model selection to chat and lets operators configure
agent task budgets from the UI.

- **Choose chat models in the composer:** select a provider and load its available
  models while chatting. Provider settings are for connecting and testing. Server
  tasks remain restricted to the model verified for tool calling.
- **Configurable task budgets:** choose up to 300 agent steps and 300 minutes in
  Settings. The default is 30 steps or 30 minutes; changing a saved preference
  affects tasks started afterward. Existing task records and provider profiles
  are retained.
- **Refreshed walkthrough:** the README now has current screenshots and an inline
  MP4 player covering SSH profiles, AI Assistant, port forwarding and proxies.

Existing vault data is retained; no database migration is required.

## New in 0.2.1

This patch improves transfer stability, the network workspace, terminal themes,
and resource management introduced in 0.2.0.

- **Transfer startup fix:** upload/download jobs now use Tauri's async runtime
  when submitted through synchronous desktop IPC, avoiding a native runtime panic.
  Regression coverage exercises both directions without an entered Tokio runtime.
- **Transfers tray:** collapse/expand the tray and clear completed, failed, or
  cancelled jobs. Clear preserves active jobs and transferred files; late events
  cannot restore cleared records.
- **Compact port forwarding:** shorter cards retain the SSH route, tunnel mode,
  listener/destination, status, active connections, byte counts, errors and actions.
- **Light terminals:** terminal backgrounds, ANSI palettes, toolbar and overlays
  follow light/dark/system theme without reconnecting or clearing output. Explicit
  RGB colors chosen by terminal programs can still override their own backgrounds.
- **Bounded output:** terminal IPC waits for xterm to consume output, with a
  256 KiB raw-output credit window per session and up to 16 sessions including
  connecting terminals. Hidden panes still acknowledge output. Closing wakes
  blocked producers, and the pane drains final output before closing.
- **Lighter chat rendering and autosave:** show 40 turns at once with navigation
  to all older turns, preserve your position while reading history, and coalesce
  autosaves behind one pending write per session. Vault locking waits for the
  latest revision. The existing transcript size limit remains unchanged.
- **Network and disk budgets:** shared 128-connection forwarding limit, retaining
  64 per tunnel; 8 MiB directory and 32 MiB transfer-plan metadata limits; and
  free-space checks for download temporary files and encrypted backups.
- **Vault maintenance:** output-preview retention runs periodically while unlocked.
  **Security → Storage** shows allocated/reusable space and offers manual
  compaction for idle vaults up to 128 MiB. Compaction retains credentials, chat,
  and history. Encryption and durability settings remain enabled.
- **Idle updates and package size:** versioned vault-status notifications reduce
  UI polling while preserving auto-lock checks; CI gates frontend and Windows
  package sizes. No runtime dependency was added for these controls.

**File conflicts:** the default **Skip matching files** skips existing names
without a prompt. **Replace matching files** asks **Transfer & replace** once for
the selected transfer, including nested files; it does not prompt per conflict.
Retry reuses the original transfer's replacement choice. A completed job may
contain skipped files. This conflict policy is unchanged from 0.2.0.

Validation includes frontend regression tests, native transfer/runtime and credit
tests, a Windows PTY stream larger than one credit window, encrypted vault
compaction/backup tests, type checking, lint and Clippy. These controls have not
yet been compared in a before/after Windows WebView CPU/RAM benchmark; no specific
memory or CPU reduction is claimed. See `docs/release/performance.md` for limits
and the scope of older measurements.

Upgrading from 0.2.0 keeps schema 9, saved profiles, credentials and history. No
new schema migration is included in this patch.

## Network workspace (introduced in 0.2.0)

SFTP, saved port forwarding, and reusable HTTP CONNECT/SOCKS5 proxies:

- **SFTP:** local/remote file browsers, recursive upload/download, byte and file
  progress, speed and ETA, a bounded queue, cancellation, retry, hidden files,
  filtering, folder creation, rename, and file/empty-folder deletion. Existing
  files are skipped by default; replacements require confirmation. Symlinks are
  skipped, and each file is finalized only after its temporary copy finishes.
- **Port forwarding:** saved Local, Remote, and Dynamic SOCKS5 TCP tunnels;
  SSH profile selection; bind and destination fields; automatic free ports;
  start/stop; live connection counts, traffic counters, and error status.
- **Proxies:** named HTTP CONNECT and SOCKS5 profiles with optional authentication;
  select a proxy in an SSH profile for terminal, AI, SFTP, and tunnels. Host-key
  verification is preserved. Proxy passwords are write-only and stored in the vault.
- **Lifecycle/performance:** file bytes stay in Rust, copy buffers are bounded,
  transfer progress is throttled, directory rows are virtualized, and new pages
  are lazy-loaded. Vault locking closes SFTP transports, cancels transfers, and
  stops tunnels. Tunnels never restart automatically after unlock or app restart.
- **SSH key management:** dedicated sidebar page, named key selection in profiles,
  Ed25519/RSA 2048/3072/4096/8192/ECDSA generation, encrypted private-key import,
  and RSA SHA-2 authentication. DSA and RSA/SHA-1 remain unsupported.
- **Host improvements:** create groups with + beside SSH hosts, drag profiles into
  groups, reveal saved passwords explicitly, preserve drafts while visiting key
  or proxy management, and close terminal tabs when their shell exits.

Upgrading from versions before 0.2.0 migrates the encrypted database to schema 9. Saved profiles and vault
credentials are retained. Create an encrypted backup before a version downgrade;
older builds cannot open a schema newer than they support. Transfer state and
running listeners are session-only. Full interactive platform/provider coverage
is not established; automated SSH network tests use local OpenSSH fixtures.

## Download

| System | File | Notes |
| --- | --- | --- |
| Windows 10/11 (x64) | `Selai_<version>_x64-setup.exe` (or the `.msi`) | SmartScreen: **More info → Run anyway**. Needs WebView2, which Windows 11 includes. |
| Linux (x64), Debian/Ubuntu | `selai_<version>_amd64.deb` | `sudo apt install ./selai_<version>_amd64.deb` |
| Linux (x64), any distro | `Selai_<version>_amd64.AppImage` | `chmod +x` it, then run. Needs a Secret Service keyring (GNOME Keyring or KWallet). |
| macOS (Apple silicon) | `Selai_<version>_aarch64.dmg` | Ad-hoc signed unless Apple signing is configured: attempt to open, then use **System Settings → Privacy & Security → Open Anyway**. |
| macOS (Intel) | `Selai_<version>_x64.dmg` | Available from 0.4.3; attempt to open, then use **System Settings → Privacy & Security → Open Anyway** for ad-hoc builds. |

## Check the download

`SHA256SUMS` lists every file in this release:

```sh
sha256sum -c SHA256SUMS --ignore-missing        # Linux
shasum -a 256 -c SHA256SUMS --ignore-missing    # macOS
```

On Windows: `Get-FileHash -Algorithm SHA256 .\<file>` and compare with `SHA256SUMS`.

## What's in it

- Hosts with labels that may contain spaces; the server key is approved inside
  the first connect, and a changed key needs an explicit confirmation.
- A desktop workbench: host explorer on the left, terminals and History/Providers
  as tabs in the middle, the AI chat on the right (Ctrl+Shift+E / Ctrl+Shift+L).
- Terminal tabs with status, reconnect, and an "Edit host" step after a failed login.
- A local terminal on this computer ("This computer"), which the AI can use under
  the same approvals as a host.
- AI Agent chat: plain technical discussion or independent tasks on multiple
  selected hosts. Follow the focused terminal, pin a host, or mention one or
  more hosts with `@`. Fixed diagnostics run on their own; other commands need
  approval, subject to the optional root grant described below.
- Commands that need root: the approval card asks for the sudo password when sudo
  asks for one (typed, saved, or the SSH login password). The model never sees it.
- An approval can enable **Allow further commands in this request on this
  host**. The in-memory grant ends with the task, cancellation,
  or vault lock; it does not cover another host or request. Commands remain
  recorded in History and the audit log.
- A dedicated **SSH Keys** page in the left sidebar: generate Ed25519,
  RSA 2048/3072/4096/8192-bit, or ECDSA P-256/P-384/P-521 key pairs,
  import private keys with an optional passphrase, view fingerprints, copy
  public keys, and select a reusable named key in each host profile. Private
  key material stays in the encrypted vault. RSA (2048–8192 bits) can be imported
  and used with SHA-2 signatures; DSA is unsupported. Profile forms use a key
  dropdown and preserve drafts when visiting key management.
- Create groups with **+** beside **SSH hosts**, then drag profiles onto a group
  or **Ungrouped**. Moves preserve credentials and connection settings; profile
  forms select existing groups without an inline group-creation flow.
- A passphrase vault: your data is encrypted at rest, with a recovery key,
  auto-lock, an encrypted backup, and encrypted `.selai-history` exports.
- Saved chat sessions, and tasks that span several hosts, each with its own approvals.
- Import SSH profiles from CSV, JSON, KeePass `.kdbx`, or OpenSSH config. Upload
  a config file or read `~/.ssh/config`, review a preview, then save selected
  profiles. Include/Match blocks and proxy connections are not imported.
  Credentials stay local.
- Chat replies render Markdown tables, code blocks and Mermaid diagrams.
- History with each request, the commands it ran, and input/output token usage.
- Providers: OpenAI, Anthropic and OpenAI-compatible servers (vLLM, Ollama,
  LiteLLM). "Check API key" loads the model list; one Test button checks the
  connection and tool calling. Plain HTTP to a gateway needs an explicit opt-in.

See `SECURITY.md` for what leaves your machine and the known limits.
`sbom-cyclonedx.tar.gz` holds the CycloneDX SBOMs.
