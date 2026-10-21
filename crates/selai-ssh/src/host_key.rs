//! Host-key observation and pin checking.

use std::sync::{Arc, Mutex};

use russh::client::{self, Handler};
use russh::keys::{HashAlg, PublicKeyOrCertificate};

use crate::{session::Endpoint, SshError};

/// A key as the server presented it. Only public material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedHostKey {
    pub algorithm: String,
    pub public_key_base64: String,
    pub fingerprint_sha256: String,
}

/// A key we trust for a host. Built from the local pin store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedHostKey {
    pub algorithm: String,
    pub public_key_base64: String,
    pub fingerprint_sha256: String,
}

impl PinnedHostKey {
    /// Parses one `known_hosts`-style public key line: `algorithm base64 [comment]`.
    pub fn from_openssh_line(line: &str) -> Result<Self, SshError> {
        let key = russh::keys::PublicKey::from_openssh(line.trim())
            .map_err(|e| SshError::Protocol(format!("invalid public key: {e}")))?;
        Ok(Self {
            algorithm: key.algorithm().as_str().to_owned(),
            public_key_base64: base64_of(&key)?,
            fingerprint_sha256: key.fingerprint(HashAlg::Sha256).to_string(),
        })
    }
}

impl ObservedHostKey {
    fn from_public_key(key: &russh::keys::PublicKey) -> Result<Self, SshError> {
        Ok(Self {
            algorithm: key.algorithm().as_str().to_owned(),
            public_key_base64: base64_of(key)?,
            fingerprint_sha256: key.fingerprint(HashAlg::Sha256).to_string(),
        })
    }
}

fn base64_of(key: &russh::keys::PublicKey) -> Result<String, SshError> {
    // OpenSSH text form is "<algorithm> <base64> [comment]". Taking field 2 is
    // exact because algorithm names and base64 contain no spaces.
    let line = key
        .to_openssh()
        .map_err(|e| SshError::Protocol(format!("cannot encode public key: {e}")))?;
    line.split_whitespace()
        .nth(1)
        .map(str::to_owned)
        .ok_or_else(|| SshError::Protocol("malformed public key encoding".to_owned()))
}

/// What the handshake saw and decided. Shared between the handler and the caller
/// because russh reports a rejected key only as a generic error.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    pub observed: Option<ObservedHostKey>,
    pub verdict: Verdict,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    #[default]
    Undecided,
    Trusted,
    Unknown,
    Changed {
        pinned_fingerprint: String,
    },
    Certificate,
}

/// Verifies the server key during key exchange. Accepts only an exact match of
/// a pinned key; every other case is rejected.
#[derive(Clone)]
pub(crate) struct PinVerifier {
    pins: Vec<PinnedHostKey>,
    outcome: Arc<Mutex<Outcome>>,
    pub(crate) remote: RemoteRoute,
}

pub(crate) type RemoteRoute =
    Arc<Mutex<Option<(u32, tokio::sync::mpsc::Sender<russh::Channel<client::Msg>>)>>>;

impl PinVerifier {
    pub(crate) fn new(pins: Vec<PinnedHostKey>) -> (Self, Arc<Mutex<Outcome>>) {
        let outcome = Arc::new(Mutex::new(Outcome::default()));
        (
            Self {
                pins,
                outcome: Arc::clone(&outcome),
                remote: Arc::new(Mutex::new(None)),
            },
            outcome,
        )
    }

    fn decide(&self, observed: &ObservedHostKey) -> Verdict {
        if let Some(pin) = self.pins.iter().find(|p| p.algorithm == observed.algorithm) {
            return if pin.public_key_base64 == observed.public_key_base64 {
                Verdict::Trusted
            } else {
                Verdict::Changed {
                    pinned_fingerprint: pin.fingerprint_sha256.clone(),
                }
            };
        }
        // The host has pins, but none for the algorithm now offered: treat as a
        // change, not as first contact.
        match self.pins.first() {
            Some(other) => Verdict::Changed {
                pinned_fingerprint: other.fingerprint_sha256.clone(),
            },
            None => Verdict::Unknown,
        }
    }
}

impl Handler for PinVerifier {
    type Error = SshError;

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        _address: &str,
        port: u32,
        _origin: &str,
        _origin_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let sender = self.remote.lock().ok().and_then(|route| {
            route
                .as_ref()
                .filter(|(p, _)| *p == 0 || *p == port)
                .map(|(_, sender)| sender.clone())
        });
        if let Some(sender) = sender {
            if sender.try_send(channel).is_ok() {
                reply.accept().await;
            }
        }
        Ok(())
    }

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let (verdict, observed) = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => {
                let observed = ObservedHostKey::from_public_key(key)?;
                (self.decide(&observed), Some(observed))
            }
            PublicKeyOrCertificate::Certificate(_) => (Verdict::Certificate, None),
        };
        let trusted = verdict == Verdict::Trusted;
        if let Ok(mut out) = self.outcome.lock() {
            out.observed = observed;
            out.verdict = verdict;
        }
        Ok(trusted)
    }
}

/// Connects far enough to read the server's host key, then disconnects.
///
/// No authentication method is ever invoked. This is how the first-trust prompt
/// obtains the fingerprint to show.
pub async fn probe_host_key(
    endpoint: &Endpoint,
    timeout: std::time::Duration,
) -> Result<ObservedHostKey, SshError> {
    probe_host_key_via(endpoint, timeout, None).await
}

pub async fn probe_host_key_via(
    endpoint: &Endpoint,
    timeout: std::time::Duration,
    proxy: Option<crate::ProxyOptions>,
) -> Result<ObservedHostKey, SshError> {
    probe_host_key_via_ref(endpoint, timeout, proxy.as_ref()).await
}

pub(crate) async fn probe_host_key_via_ref(
    endpoint: &Endpoint,
    timeout: std::time::Duration,
    proxy: Option<&crate::ProxyOptions>,
) -> Result<ObservedHostKey, SshError> {
    let (verifier, outcome) = PinVerifier::new(Vec::new());
    let config = Arc::new(client::Config::default());
    let attempt = async {
        let stream = crate::proxy::transport(endpoint, proxy).await?;
        client::connect_stream(config, stream, verifier).await
    };
    let result = tokio::time::timeout(timeout, attempt).await;

    let guard = outcome
        .lock()
        .map_err(|_| SshError::Protocol("probe state poisoned".to_owned()))?;
    match result {
        Err(_) => Err(SshError::ConnectTimeout),
        Ok(Ok(_handle)) => Err(SshError::Protocol(
            "server accepted without a host key check".to_owned(),
        )),
        Ok(Err(err)) => match guard.observed.clone() {
            Some(observed) => Ok(observed),
            None => Err(map_connect_error(err)),
        },
    }
}

pub(crate) fn map_connect_error(err: SshError) -> SshError {
    match err {
        SshError::ConnectFailed(ref msg) if msg.contains("lookup") => SshError::Dns,
        other => other,
    }
}
