//! SSH transport for Selai (ADR-0002).
//!
//! Two operations matter for security:
//! - [`probe_host_key`] reads the server's host key and never authenticates.
//! - [`connect`] authenticates only after the server key equals a pinned key.
//!
//! Anything else (unknown key, changed key, certificate) is rejected during key
//! exchange, before a credential is sent.

mod error;
mod native;
pub use native::{NativeAuth, NativeInteraction, SshStream};
mod exec;
mod forward;
mod host_key;
mod private_key;
mod proxy;
mod session;
mod shell;
pub mod sudo;

pub use error::SshError;
pub use exec::{ExecEnd, ExecOutput};
pub use forward::{ForwardCounters, ForwardSpec, Forwarder};
pub use host_key::{probe_host_key, probe_host_key_via, ObservedHostKey, PinnedHostKey};
pub use private_key::{generate_rsa_key, validate_private_key};
pub use proxy::{ProxyKind, ProxyOptions};
pub use session::{connect, connect_via, Auth, ConnectOptions, Endpoint, Session};
pub use shell::{Shell, ShellEvent};
pub use sudo::SudoStatus;
