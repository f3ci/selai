# Vault validation — 2026-10-16

All data used for this validation was synthetic. The user's real Selai database,
saved credentials, and history were not migrated or modified.

## Verified

| Check | Result |
| --- | --- |
| Frontend Vitest | 94 passed across 15 files |
| TypeScript, ESLint, Vite production bundle | Passed; existing large Mermaid chunk warning remains |
| Desktop Rust unit tests (Debian/WSL) | 20 passed |
| Storage unit tests | 24 passed |
| Agent runtime with mock provider/executor | 28 passed |
| Vault integration tests | 4 passed |
| Rust Clippy for changed packages, libraries and tests (`-D warnings`) | Passed on Linux |
| Windows platform adapter compilation and Clippy | Passed |
| Windows API compatibility probe | Current-session unlock query and temporary power-notification registration succeeded |

Coverage includes ciphertext/WAL inspection, wrong keys and passphrases,
authenticated history tampering, credential migration, retained metadata and
exports, migration failure preserving sources, pending credential-cleanup
tombstones, changed source files not being deleted, managed private keys,
backup/restore with passphrase or recovery key, recovery rotation, passphrase
change, idle admission, cancellation of network operations, live terminal standby
and reuse after unlock, final transcript persistence, and inaccessible DB
references after lock.
The graceful drain test proves the current command's completed result is retained
and the following command never starts.

Reproduce on a machine with the documented native build prerequisites:

```sh
cd apps/desktop
pnpm typecheck
pnpm lint
pnpm test --maxWorkers=2
pnpm build
cd ../..
cargo fmt --all -- --check
cargo test -p selai-desktop -p selai-agent -p selai-storage -p selai-vault --lib --test runtime --test vault -- --test-threads=1
cargo clippy -p selai-storage -p selai-vault -p selai-agent -p selai-desktop -p selai-platform --lib --tests -- -D warnings
```

On a constrained test runner, `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, and `CARGO_INCREMENTAL=0` reduce build-cache space.
These are test-build settings, not application runtime settings.

## Windows setup stack overflow

The Windows CI run `38931811850` aborted a vault lifecycle test with
`STATUS_STACK_OVERFLOW` (`0xc00000fd`). The installed release executable also
reproduced the crash during setup in a diagnostic copy with a distinct Tauri
identifier and fresh synthetic data. The captured stack repeatedly entered
SQLCipher's native logger and memory allocation/locking callbacks.

SQLCipher formats native warnings using SQLite's allocator. When Windows refuses
to lock another memory page, logging that warning can recursively allocate and
attempt to lock more pages while `cipher_memory_security` is enabled. Encrypted
connections now set `cipher_log_level=NONE` before enabling memory security.
SQLite errors still return through the application API.

A second diagnostic copy of the same Windows executable, with the native default
log level set to `NONE`, completed setup, generated a recovery key, saved a
synthetic SSH profile with a credential, locked, unlocked, and retained the
profile. This is a controlled native runtime check; it is not a rebuilt MSI from
the fixed source. No real user vault was opened or changed.

The storage regression writes and reopens a 1 MiB encrypted credential while
checking that memory security remains enabled and native logging is disabled.
The fixed source passed all 68 storage and vault tests on Linux, including this
regression and encrypted backup/recovery coverage.
Windows packaging now runs that regression, the crypto integration suite, and
the vault lifecycle suite in the release profile before uploading installers.

## Remaining manual/platform validation

- A rebuilt Windows MSI from the fixed source and the complete Windows UI
  unlock/lock/sleep flow still need validation. The native diagnostic above
  covers the setup, persistence, lock and unlock backend path.
- Native file dialogs and the visual layout need a manual desktop check.
  Browser automation preview was unavailable in this environment.
- The full SSH/sudo/keyring integration suite was not completed. An attempted
  `local_e2e` run passed three tests, including a real local PTY, and failed three
  environment-dependent cases requiring root/sshd or passwordless sudo. Use
  the root/keyring harness in CONTRIBUTING.md and CI for the complete suite.
- Linux/macOS OS session-lock and suspend integration remains unimplemented;
  manual and idle vault locking are supported.
- No independent cryptographic audit or runtime CPU/RAM benchmark is claimed.
