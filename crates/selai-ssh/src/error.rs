use selai_domain::{ErrorCode, SelaiError};

use crate::ObservedHostKey;

#[derive(Debug, thiserror::Error)]
pub enum SshError {
    #[error("host name could not be resolved")]
    Dns,
    #[error("connection timed out")]
    ConnectTimeout,
    #[error("could not connect: {0}")]
    ConnectFailed(String),
    /// First contact: the server key is not pinned yet. The caller shows the
    /// fingerprint and may pin it through the trust flow.
    #[error("host key is not trusted yet")]
    HostKeyUnknown(ObservedHostKey),
    /// The server presented a different key than the one pinned for this host.
    #[error("host key does not match the pinned key")]
    HostKeyChanged {
        observed: ObservedHostKey,
        pinned_fingerprint: String,
    },
    /// Server offered a host certificate; certificates are not accepted in v1.
    #[error("server presented a host certificate, which v1 does not accept")]
    UnsupportedHostKey,
    #[error("authentication failed")]
    AuthFailed,
    #[error("OpenSSH authentication failed: {0}")]
    NativeAuth(String),
    #[error("private key could not be loaded: {0}")]
    KeyLoad(String),
    #[error("connection closed")]
    ChannelClosed,
    #[error("protocol error: {0}")]
    Protocol(String),
}

impl SelaiError for SshError {
    fn code(&self) -> ErrorCode {
        match self {
            Self::Dns => ErrorCode::SshDns,
            Self::ConnectTimeout => ErrorCode::SshConnectTimeout,
            Self::ConnectFailed(_) | Self::Protocol(_) => ErrorCode::SshConnectFailed,
            Self::HostKeyUnknown(_) | Self::UnsupportedHostKey => ErrorCode::SshHostkeyUnknown,
            Self::HostKeyChanged { .. } => ErrorCode::SshHostkeyChanged,
            Self::AuthFailed | Self::NativeAuth(_) | Self::KeyLoad(_) => ErrorCode::SshAuthFailed,
            Self::ChannelClosed => ErrorCode::SshChannelClosed,
        }
    }
}

impl From<russh::Error> for SshError {
    fn from(err: russh::Error) -> Self {
        use russh::Error as E;
        match err {
            E::IO(e) => Self::ConnectFailed(e.to_string()),
            E::ConnectionTimeout | E::Elapsed(_) => Self::ConnectTimeout,
            E::KeepaliveTimeout | E::InactivityTimeout | E::Disconnect | E::HUP | E::SendError => {
                Self::ChannelClosed
            }
            E::Keys(e) => Self::KeyLoad(e.to_string()),
            other => Self::Protocol(other.to_string()),
        }
    }
}

impl From<russh::SendError> for SshError {
    fn from(_: russh::SendError) -> Self {
        Self::ChannelClosed
    }
}

impl From<std::io::Error> for SshError {
    fn from(err: std::io::Error) -> Self {
        Self::ConnectFailed(err.to_string())
    }
}
