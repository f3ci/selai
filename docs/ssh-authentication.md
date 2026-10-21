# SSH authentication

Password and managed private-key profiles use Selai's embedded SSH transport.
Additional authentication methods use the operating system's OpenSSH client.
Select them in **Edit host → Additional authentication methods**.

| Selection | Credentials / prerequisites |
| --- | --- |
| Automatic / multi-factor | OpenSSH defaults and the user's SSH configuration; supports server-required combinations and `none` where explicitly allowed by the server |
| Keyboard-interactive / OTP | Live prompts for password, verification codes, challenges and expired-password changes |
| SSH agent | A running agent with loaded keys; includes agents supplied by password managers and agent-backed certificates or hardware keys |
| OpenSSH user certificate | A local private key plus its `<key-path>-cert.pub` certificate, or `CertificateFile` configured for the destination; agent-held certificates can use SSH agent mode |
| FIDO2 / security key | An OpenSSH build with FIDO support, the physical token, and a security-key identity file or agent; OpenSSH handles touch/PIN requests |
| Smart card / PKCS#11 | A compatible OpenSSH build and provider DLL/shared library; specify its path or configure `PKCS11Provider` |
| Kerberos / GSSAPI | A GSSAPI-capable OpenSSH build, a valid ticket and server service principal; use its matching DNS name |
| Host-based | An OpenSSH build supporting host-based authentication, `ssh-keysign`, local host keys and server-side trust |

Each native selection tries its primary method first, then permits
keyboard-interactive/password for additional MFA steps or fallback offered by
the server. Choose Automatic for other method orderings. Certificate and FIDO
selections restrict public-key algorithms to the selected key type; smart-card
mode obtains keys from its PKCS#11 provider rather than ordinary identity files.
Authentication dialogs include transient server instructions, and updates do
not clear a response the operator is typing. Native credentials are requested live; passwords, PINs, OTPs and answers are not
saved in profiles, logs, command arguments or environment variables. Prompt
responses are bound to a host and an expiring, single-use request. Cancellation,
vault lock and session disposal revoke pending requests.

OpenSSH is normally `/usr/bin/ssh` on Linux/macOS and
`%SystemRoot%\System32\OpenSSH\ssh.exe` on Windows. Set
`SELAI_SSH_EXECUTABLE` to an **absolute path** before launching Selai to use
another trusted OpenSSH build. Feature availability follows that build: merely
selecting Kerberos, PKCS#11 or FIDO does not install an OS provider, acquire a
ticket or provision a token. Local identity files retain OpenSSH's normal file
permission checks. Keys stored only in Selai's vault are used by Private key
mode; native profiles use local identity files or an external SSH agent.

OpenSSH reads the user's SSH config for the destination entered in the profile.
IdentityAgent, CertificateFile, PKCS11Provider, SecurityKeyProvider and the
platform's agent/provider configuration continue to apply. Selai fixes the
host, user and port from the profile and supplies an isolated known_hosts file
containing only that host's trusted keys, in a private directory (0700 on Unix,
restricted to the current account on Windows). It enforces strict verification,
disables DNS key trust/key updates, and never changes the user's known_hosts.
The existing Selai probe/review/trust flow runs before authentication. HTTP and
SOCKS proxy profiles use an authenticated local stdio relay; proxy credentials
remain in the Selai process. Configure network routes in Selai rather than
relying on OpenSSH-only ProxyJump routing for the initial host-key probe.

The native backend works with terminal PTYs, SFTP, AI command execution, and
local, dynamic and remote forwarding. Each operation uses a separate OpenSSH
connection; servers requiring MFA may prompt again when another operation
starts. The native helper acknowledges completed authentication before SFTP or
command execution timeouts begin, so OTP/PIN entry does not consume those
operation budgets. The connection check authenticates with `ssh -N` and executes no remote
command. AI commands still pass through Selai's existing approval/audit path.
Closing a session terminates its OpenSSH processes. As with the embedded
backend, interrupting an SSH process does not guarantee termination of a remote
command that has already started.

This adds **user authentication**. It does not add OpenSSH host-CA trust or
change Selai's host-key pinning policy. Native settings do not enable legacy
SHA-1/DSA authentication; the chosen OpenSSH build's normal user-key policy
applies, and host RSA negotiation is restricted to SHA-2.

## Validation

Automated tests exercise the actual Windows OpenSSH executable against an
isolated SSH server for identity/certificate authentication, terminal output,
command stdout/stderr/exit status, SFTP, direct TCP forwarding, remote port
allocation, and host-key rejection. Separate tests cover the password-only
embedded fallback, profile persistence, prompt responses/cancellation/revocation,
and frontend MFA queuing and secret clearing. Physical FIDO/PKCS#11 devices,
external password-manager agents, Kerberos domains and host-based server trust
require validation in their respective environments; they are integration
paths, not claims of completed live tests for those systems.
