//! Authenticated SSH sessions.

use std::{path::PathBuf, sync::Arc, time::Duration};

use russh::{
    client::{self},
    keys::PrivateKeyWithHashAlg,
    Disconnect,
};
use selai_secrets::SecretValue;

use crate::{
    host_key::{map_connect_error, Outcome, PinVerifier, Verdict},
    PinnedHostKey, Shell, SshError,
};

/// Host and port. The host is a DNS name or an IP literal without brackets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }

    pub(crate) fn socket_addr_pair(&self) -> (String, u16) {
        (self.host.clone(), self.port)
    }
}

/// Credential for one connection. Secrets are only read here, never stored.
pub enum Auth {
    Native(crate::NativeAuth),
    Password(SecretValue),
    PrivateKey {
        path: PathBuf,
        passphrase: Option<SecretValue>,
    },
    PrivateKeyData {
        pem: SecretValue,
        passphrase: Option<SecretValue>,
    },
}

pub struct ConnectOptions {
    pub endpoint: Endpoint,
    pub username: String,
    pub auth: Auth,
    /// Keys trusted for this host. Empty means no key is trusted, so every
    /// connection is refused before authentication.
    pub pinned: Vec<PinnedHostKey>,
    pub connect_timeout: Duration,
    pub keepalive: Option<Duration>,
}

/// An authenticated transport. Terminal and exec channels are opened on it.
pub struct Session {
    pub(crate) handle: Option<client::Handle<PinVerifier>>,
    pub(crate) native: Option<Arc<crate::native::NativeSession>>,
    endpoint: Endpoint,
    remote: crate::host_key::RemoteRoute,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(native) = &self.native {
            native.stop.cancel();
        }
    }
}

impl Session {
    pub(crate) fn channel_timeout(&self) -> Duration {
        Duration::from_secs(if self.native.is_some() { 300 } else { 15 })
    }
    pub(crate) fn handle(&self) -> Result<&client::Handle<PinVerifier>, SshError> {
        self.handle.as_ref().ok_or(SshError::ChannelClosed)
    }
    pub fn is_closed(&self) -> bool {
        if let Some(native) = &self.native {
            return native.is_closed();
        }
        self.handle.as_ref().is_none_or(|handle| handle.is_closed())
    }
    pub async fn close(&self) {
        if let Some(native) = &self.native {
            native.stop.cancel();
            return;
        }
        let Ok(handle) = self.handle() else {
            return;
        };
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
    }

    pub async fn open_sftp(&self) -> Result<russh_sftp::client::SftpSession, SshError> {
        let stream = self.sftp_stream().await?;
        russh_sftp::client::SftpSession::new(stream)
            .await
            .map_err(|e| SshError::Protocol(format!("SFTP could not start: {e}")))
    }
    pub async fn sftp_stream(&self) -> Result<crate::SshStream, SshError> {
        if let Some(native) = &self.native {
            return native.stream(&["-s"], Some("sftp")).await;
        }
        let channel = self.handle()?.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;
        Ok(Box::new(channel.into_stream()))
    }

    pub async fn direct_tcpip(&self, host: &str, port: u16) -> Result<crate::SshStream, SshError> {
        if let Some(native) = &self.native {
            let target = if host.contains(':') {
                format!("[{host}]:{port}")
            } else {
                format!("{host}:{port}")
            };
            return native.stream(&["-W", &target], None).await;
        }
        Ok(Box::new(
            self.handle()?
                .channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0)
                .await?
                .into_stream(),
        ))
    }

    pub async fn remote_forward(
        &self,
        bind: &str,
        port: u16,
    ) -> Result<(u16, tokio::sync::mpsc::Receiver<crate::SshStream>), SshError> {
        if let Some(native) = &self.native {
            return native.remote_forward(bind, port).await;
        }
        let (tx, mut rx) = tokio::sync::mpsc::channel::<russh::Channel<client::Msg>>(32);
        if let Ok(mut route) = self.remote.lock() {
            *route = Some((u32::from(port), tx));
        }
        let assigned = self.handle()?.tcpip_forward(bind, u32::from(port)).await?;
        let actual = if port == 0 {
            u16::try_from(assigned).map_err(|_| SshError::Protocol("Invalid remote port".into()))?
        } else {
            port
        };
        if let Ok(mut route) = self.remote.lock() {
            if let Some((p, _)) = route.as_mut() {
                *p = u32::from(actual);
            }
        }
        let (stream_tx, stream_rx) = tokio::sync::mpsc::channel(32);
        tokio::spawn(async move {
            loop {
                tokio::select! {_=stream_tx.closed()=>break,channel=rx.recv()=>match channel {Some(channel)=>if stream_tx.send(Box::new(channel.into_stream()) as crate::SshStream).await.is_err(){break},None=>break}}
            }
        });
        Ok((actual, stream_rx))
    }

    pub async fn cancel_remote_forward(&self, bind: &str, port: u16) {
        if let Some(native) = &self.native {
            native.cancel_remote_forward(bind, port);
            return;
        }
        let Ok(handle) = self.handle() else {
            return;
        };
        let _ = handle.cancel_tcpip_forward(bind, u32::from(port)).await;
        if let Ok(mut route) = self.remote.lock() {
            *route = None;
        }
    }
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Opens a PTY channel with a login shell.
    pub async fn open_shell(&self, cols: u32, rows: u32, term: &str) -> Result<Shell, SshError> {
        if let Some(native) = &self.native {
            return Ok(Shell::from_native(native.open_shell(cols, rows, term)?));
        }
        let channel = self.handle()?.channel_open_session().await?;
        channel
            .request_pty(true, term, cols, rows, 0, 0, &[])
            .await?;
        channel.request_shell(true).await?;
        Ok(Shell::new(channel))
    }

    pub async fn disconnect(self) -> Result<(), SshError> {
        if let Some(native) = &self.native {
            native.stop.cancel();
            return Ok(());
        }
        self.handle()?
            .disconnect(Disconnect::ByApplication, "", "en")
            .await?;
        Ok(())
    }
}

/// Connects, verifies the host key against the pins, and authenticates.
///
/// The whole sequence (TCP, key exchange, authentication) is bounded by
/// `connect_timeout`.
pub async fn connect(opts: ConnectOptions) -> Result<Session, SshError> {
    connect_via(opts, None).await
}

pub async fn connect_via(
    opts: ConnectOptions,
    proxy: Option<crate::ProxyOptions>,
) -> Result<Session, SshError> {
    if matches!(&opts.auth, Auth::Native(_)) {
        let native = crate::native::NativeSession::connect(&opts, proxy).await?;
        let verifier = PinVerifier::new(opts.pinned.clone()).0;
        return Ok(Session {
            handle: None,
            native: Some(native),
            endpoint: opts.endpoint,
            remote: verifier.remote.clone(),
        });
    }
    let (verifier, outcome) = PinVerifier::new(opts.pinned.clone());
    let remote = verifier.remote.clone();
    let config = Arc::new(client::Config {
        keepalive_interval: opts.keepalive,
        ..client::Config::default()
    });
    let endpoint = opts.endpoint.clone();

    let attempt = async {
        let stream = crate::proxy::transport(&endpoint, proxy.as_ref()).await?;
        let mut handle = client::connect_stream(config, stream, verifier).await?;
        authenticate(&mut handle, &opts.username, &opts.auth).await?;
        Ok::<_, SshError>(handle)
    };

    match tokio::time::timeout(opts.connect_timeout, attempt).await {
        Err(_) => Err(SshError::ConnectTimeout),
        Ok(Ok(handle)) => Ok(Session {
            handle: Some(handle),
            native: None,
            endpoint,
            remote,
        }),
        Ok(Err(err)) => Err(explain(err, &outcome)),
    }
}

async fn authenticate(
    handle: &mut client::Handle<PinVerifier>,
    username: &str,
    auth: &Auth,
) -> Result<(), SshError> {
    let result = match auth {
        Auth::Native(_) => return Err(SshError::AuthFailed),
        Auth::Password(secret) => {
            let password = utf8(secret.expose_for_transport()).ok_or(SshError::AuthFailed)?;
            let result = handle.authenticate_password(username, password).await?;
            if let client::AuthResult::Failure {
                remaining_methods,
                partial_success: false,
            } = &result
            {
                if remaining_methods.contains(&russh::MethodKind::KeyboardInteractive) {
                    return authenticate_interactive_password(handle, username, password).await;
                }
            }
            result
        }
        Auth::PrivateKey { passphrase, .. } | Auth::PrivateKeyData { passphrase, .. } => {
            let passphrase = match passphrase {
                Some(p) => Some(
                    utf8(p.expose_for_transport())
                        .ok_or_else(|| SshError::KeyLoad("passphrase is not UTF-8".to_owned()))?,
                ),
                None => None,
            };
            let key = match auth {
                Auth::PrivateKey { path, .. } => russh::keys::load_secret_key(path, passphrase),
                Auth::PrivateKeyData { pem, .. } => russh::keys::decode_secret_key(
                    utf8(pem.expose_for_transport())
                        .ok_or_else(|| SshError::KeyLoad("Private key is not UTF-8".into()))?,
                    passphrase,
                ),
                Auth::Password(_) | Auth::Native(_) => unreachable!(),
            }
            .map_err(|e| SshError::KeyLoad(e.to_string()))?;
            if matches!(key.algorithm(), russh::keys::Algorithm::Rsa { .. }) {
                let mut signer = crate::private_key::RsaSigner::new(&key)?;
                let hashes = match handle.best_supported_rsa_hash().await? {
                    // Some servers advertise SHA-512 globally even when the
                    // account's policy only permits SHA-256. Retry SHA-256 on
                    // an authentication refusal, never SHA-1.
                    Some(Some(russh::keys::HashAlg::Sha512)) => vec![
                        russh::keys::HashAlg::Sha512, russh::keys::HashAlg::Sha256,
                    ],
                    Some(Some(hash)) => vec![hash],
                    Some(None) => return Err(SshError::KeyLoad(
                        "Server only supports legacy ssh-rsa/SHA-1. Enable rsa-sha2-256 or rsa-sha2-512 on the server, or use an Ed25519/ECDSA key.".into()
                    )),
                    // Older servers may omit server-sig-algs but accept SHA-2.
                    None => vec![russh::keys::HashAlg::Sha512, russh::keys::HashAlg::Sha256],
                };
                for hash in hashes {
                    let result = handle
                        .authenticate_publickey_with(
                            username,
                            key.public_key().clone(),
                            Some(hash),
                            &mut signer,
                        )
                        .await?;
                    if result.success() {
                        return Ok(());
                    }
                }
                return Err(SshError::AuthFailed);
            }
            crate::validate_private_key(&key)?;
            handle
                .authenticate_publickey(username, PrivateKeyWithHashAlg::new(Arc::new(key), None))
                .await?
        }
    };
    if result.success() {
        Ok(())
    } else {
        Err(SshError::AuthFailed)
    }
}

/// Password-only keyboard-interactive fallback. Do not reuse the saved password
/// for OTP, password changes, multiple prompts, or repeated password challenges.
async fn authenticate_interactive_password(
    handle: &mut client::Handle<PinVerifier>,
    username: &str,
    password: &str,
) -> Result<(), SshError> {
    use client::KeyboardInteractiveAuthResponse as Response;

    let mut response = handle
        .authenticate_keyboard_interactive_start(username, None)
        .await?;
    let mut password_sent = false;
    // PAM may send empty informational rounds before or after the password.
    // Bound these independently of the connection timeout.
    for _ in 0..8 {
        let answers = match response {
            Response::Success => return Ok(()),
            Response::Failure { .. } => return Err(SshError::AuthFailed),
            Response::InfoRequest { prompts, .. } if prompts.is_empty() => Vec::new(),
            Response::InfoRequest { prompts, .. }
                if !password_sent
                    && prompts.len() == 1
                    && !prompts[0].echo
                    && prompts[0]
                        .prompt
                        .trim()
                        .trim_end_matches(':')
                        .trim()
                        .eq_ignore_ascii_case("password") =>
            {
                password_sent = true;
                vec![password.to_owned()]
            }
            Response::InfoRequest { .. } => return Err(SshError::AuthFailed),
        };
        response = handle
            .authenticate_keyboard_interactive_respond(answers)
            .await?;
    }
    Err(SshError::AuthFailed)
}

/// Turns a failed connect into the most specific error. Key verdicts come from
/// the handshake outcome, because russh reports a rejected key only generically.
fn explain(err: SshError, outcome: &std::sync::Mutex<Outcome>) -> SshError {
    let Ok(out) = outcome.lock() else {
        return map_connect_error(err);
    };
    match (&out.verdict, out.observed.clone()) {
        (Verdict::Unknown, Some(observed)) => SshError::HostKeyUnknown(observed),
        (Verdict::Changed { pinned_fingerprint }, Some(observed)) => SshError::HostKeyChanged {
            observed,
            pinned_fingerprint: pinned_fingerprint.clone(),
        },
        (Verdict::Certificate, _) => SshError::UnsupportedHostKey,
        _ => map_connect_error(err),
    }
}

fn utf8(bytes: &[u8]) -> Option<&str> {
    std::str::from_utf8(bytes).ok()
}
