# ADR-0006: Passphrase-protected local vault

Status: Accepted. Supersedes ADR-0003 for production credential storage.

## Storage and key hierarchy

The production composition root starts locked. A random 256-bit data key
protects the entire database with bundled SQLCipher, including WAL, metadata,
credentials, managed private keys, chat, settings, audit, and retained exports.
SQLite temporary storage is in memory. SQLCipher memory security is enabled.
There is no keyring fallback during normal operation.

`vault.json` contains a versioned header, random database filename, Argon2id
parameters/salt, and two authenticated AES-256-GCM wrappings of the data key.
One wrapping uses a passphrase key (Argon2id v19, 64 MiB, 3 iterations, 1 lane),
the other a random 256-bit recovery key. They have independent nonces and AAD.
Header parsing bounds KDF costs and forbids paths outside the data directory.
The passphrase, recovery key, and plaintext data key are never persisted by the
backend. Rust key buffers are zeroized on drop; JS/OS/library copies do not carry
a promise of forensic erasure.

Changing the passphrase rewraps the existing data key. Recovery verifies the
encrypted database before atomically replacing the header, then rotates the
recovery key. Old backups remain accessible with their old credentials.

## Lifecycle and admission

All data commands acquire an operation lease through VaultManager. Locking
closes admission first, drains the current agent command for up to 65 seconds,
cancels remaining tasks, and closes SFTP/forwarding connections. Authenticated
terminal transports and bounded terminal buffers remain in standby across lock.
Terminal input and resize operations require an unlocked vault; output credit
acknowledgements remain available without a data lease. The database is sealed
only after tasks, network operations, and operation leases finish. The renderer has up to
three seconds to persist its final transcript through a write-only snapshot
path; a hung renderer cannot indefinitely defer locking. Unflushed display-only
tokens can be lost in that exceptional case; execution/audit state is stored
independently by the backend. Any surviving DB Arc refers to an empty in-memory
connection after sealing. Reopening reconciles interrupted work without replay.
Only terminal components remain mounted behind the hidden, inert lock screen;
forms, chat, and other data views unmount. Terminal output and transport session
keys therefore remain in process memory while locked. Output parsing pauses
until unlock; byte credits bound queued output and apply backpressure. Closing a terminal tab or
exiting Selai ends that transport; the server or network can still disconnect it.

Auto-lock uses monotonic time since user interaction. Windows lock state is
polled every 250 ms using the current process's WTS session, and native power
callbacks latch a lock request. The backend monitor begins locking within its
one-second interval; admission rejects latched system lock requests immediately.
A suspended process cannot finish releasing keys until it is scheduled again.
Unknown Windows lock state fails closed. Linux/macOS currently support manual
and idle locking; OS lock/suspend integration is pending. The small Win32 FFI
adapter is isolated in selai-platform; other crates continue forbidding unsafe.

## Explicit profile password reveal

The unlocked host editor may retrieve a saved SSH login password only through
`host_reveal_password`. This is a deliberate exception to ADR-0003's original
no-read-IPC rule. The normal operation lease checks lock/locking/idle state;
local, archived, and key-authentication profiles are refused. Ordinary host
DTOs never carry a password. The UI requests it on the eye-button click and
clears preview state on hide, blur/visibility loss, vault-lock flushing, editor teardown, or a
30-second timeout. It never treats a revealed preview as a password edit.
Private keys and all other stored credential types remain outside this API.
No reveal value is persisted or included in chat or audit. JS/runtime copies
retain the memory-erasure limitations described above.

## Migration and files

One process owns `vault.lock`. The old application must be closed before setup.
Migration uses SQLCipher export to a new generation, copies legacy credentials
into that encrypted database, imports available key files, and archives JSON
exports. Integrity/checkpoint validation precedes manifest commit. Required
missing credentials or unavailable keyring access abort without deleting sources.
Cleanup deletes only copied credentials and unchanged copied files. Cleanup
failures remain visible and retryable. Tombstones retain pending keyring cleanup
even if a credential is subsequently forgotten. External private key originals,
user export files elsewhere, OS backups, and SSD remnants cannot be erased by
this migration. Password managers with their own export formats remain external.

Backups contain the wrapped-key header and a consistent encrypted SQLCipher
snapshot. Restore validates into a new generation before committing the header
and requires a fresh directory, preventing accidental replacement of current
data. Recovery-key restore works without the old passphrase and generates a new
passphrase wrapping and recovery key. History documents use AES-256-GCM with a distinct AAD and carry a wrapped
header for portability. Native dialogs choose export destinations; plaintext is
written only by an explicit decrypt action. Export paths cannot target the
application data folder. Display preferences alone remain in localStorage.
