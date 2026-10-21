//! Validated host profile fields (engineering spec section 3.1).
//!
//! Every value that crosses into storage or transport passes through one of
//! these types. Parsing is strict: the storage layer never re-validates.

use std::net::{Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

use crate::{ErrorCode, SelaiError};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostFieldError {
    #[error("hostname is not a valid DNS name or IP address")]
    Hostname,
    #[error("username must be 1 to 64 printable characters without spaces")]
    Username,
    #[error("connect timeout must be between 1 and 120 seconds")]
    ConnectTimeout,
    #[error("keepalive must be between 0 and 300 seconds")]
    Keepalive,
    #[error("terminal type must be 1 to 64 characters from [A-Za-z0-9._-]")]
    TerminalType,
    #[error("notes must be at most 4096 bytes and contain no control characters except newline")]
    Notes,
    #[error("private key path must be non-empty and contain no control characters")]
    KeyPath,
    #[error("group name must be 1 to 64 characters")]
    GroupName,
}

impl SelaiError for HostFieldError {
    fn code(&self) -> ErrorCode {
        ErrorCode::InvalidInput
    }
}

/// A DNS name or an IP literal, stored without brackets.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hostname(String);

impl Hostname {
    pub fn parse(input: &str) -> Result<Self, HostFieldError> {
        let trimmed = input.trim();
        if trimmed.is_empty() || trimmed != input {
            return Err(HostFieldError::Hostname);
        }
        let bare = trimmed
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(trimmed);
        if bare.parse::<Ipv4Addr>().is_ok() || bare.parse::<Ipv6Addr>().is_ok() {
            return Ok(Self(bare.to_owned()));
        }
        if is_dns_name(bare) {
            Ok(Self(bare.to_ascii_lowercase()))
        } else {
            Err(HostFieldError::Hostname)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_dns_name(name: &str) -> bool {
    if name.len() > 253 {
        return false;
    }
    name.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

impl Serialize for Hostname {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Hostname {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// SSH login name: 1 to 64 printable ASCII characters, no spaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Username(String);

impl Username {
    pub fn parse(input: &str) -> Result<Self, HostFieldError> {
        let ok =
            (1..=64).contains(&input.len()) && input.bytes().all(|b| (0x21..=0x7e).contains(&b));
        if ok {
            Ok(Self(input.to_owned()))
        } else {
            Err(HostFieldError::Username)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Seconds, bounded to 1..=120.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectTimeout(u16);

impl ConnectTimeout {
    pub const DEFAULT: Self = Self(15);

    pub fn new(seconds: u32) -> Result<Self, HostFieldError> {
        match u16::try_from(seconds) {
            Ok(v @ 1..=120) => Ok(Self(v)),
            _ => Err(HostFieldError::ConnectTimeout),
        }
    }

    pub fn seconds(self) -> u16 {
        self.0
    }
}

/// Seconds, bounded to 0..=300. Zero disables keepalive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keepalive(u16);

impl Keepalive {
    pub const DEFAULT: Self = Self(30);

    pub fn new(seconds: u32) -> Result<Self, HostFieldError> {
        match u16::try_from(seconds) {
            Ok(v @ 0..=300) => Ok(Self(v)),
            _ => Err(HostFieldError::Keepalive),
        }
    }

    pub fn seconds(self) -> u16 {
        self.0
    }
}

/// Terminal type sent in `pty-req`, for example `xterm-256color`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalType(String);

impl TerminalType {
    pub const DEFAULT: &'static str = "xterm-256color";

    pub fn parse(input: &str) -> Result<Self, HostFieldError> {
        let ok = (1..=64).contains(&input.len())
            && input
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if ok {
            Ok(Self(input.to_owned()))
        } else {
            Err(HostFieldError::TerminalType)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Free text notes: at most 4096 bytes, no control characters except `\n`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notes(String);

impl Notes {
    pub const MAX_BYTES: usize = 4096;

    pub fn parse(input: &str) -> Result<Self, HostFieldError> {
        let ok =
            input.len() <= Self::MAX_BYTES && input.chars().all(|c| c == '\n' || !c.is_control());
        if ok {
            Ok(Self(input.to_owned()))
        } else {
            Err(HostFieldError::Notes)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Group display name: 1 to 64 characters, no control characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupName(String);

impl GroupName {
    pub fn parse(input: &str) -> Result<Self, HostFieldError> {
        let trimmed = input.trim();
        let len = trimmed.chars().count();
        if (1..=64).contains(&len) && !input.chars().any(char::is_control) {
            Ok(Self(trimmed.to_owned()))
        } else {
            Err(HostFieldError::GroupName)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Private key file path. Stored as given; its content is read locally only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPath(String);

impl KeyPath {
    pub fn parse(input: &str) -> Result<Self, HostFieldError> {
        if !input.is_empty() && !input.chars().any(char::is_control) {
            Ok(Self(input.to_owned()))
        } else {
            Err(HostFieldError::KeyPath)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Password,
    PrivateKey,
    Auto,
    KeyboardInteractive,
    Agent,
    Certificate,
    SecurityKey,
    SmartCard,
    Gssapi,
    HostBased,
}

impl AuthMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::PrivateKey => "private_key",
            Self::Auto => "auto",
            Self::KeyboardInteractive => "keyboard_interactive",
            Self::Agent => "agent",
            Self::Certificate => "certificate",
            Self::SecurityKey => "security_key",
            Self::SmartCard => "smart_card",
            Self::Gssapi => "gssapi",
            Self::HostBased => "host_based",
        }
    }

    pub fn uses_native(self) -> bool {
        !matches!(self, Self::Password | Self::PrivateKey)
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "password" => Some(Self::Password),
            "private_key" => Some(Self::PrivateKey),
            "auto" => Some(Self::Auto),
            "keyboard_interactive" => Some(Self::KeyboardInteractive),
            "agent" => Some(Self::Agent),
            "certificate" => Some(Self::Certificate),
            "security_key" => Some(Self::SecurityKey),
            "smart_card" => Some(Self::SmartCard),
            "gssapi" => Some(Self::Gssapi),
            "host_based" => Some(Self::HostBased),
            _ => None,
        }
    }
}

/// Everything a user can set on a host, already validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSettings {
    pub label: crate::HostLabel,
    pub hostname: Hostname,
    pub port: crate::Port,
    pub username: Username,
    pub auth: AuthMethod,
    pub private_key_path: Option<KeyPath>,
    pub group_id: Option<crate::HostGroupId>,
    pub connect_timeout: ConnectTimeout,
    pub keepalive: Keepalive,
    pub terminal_type: TerminalType,
    pub notes: Option<Notes>,
}

impl HostSettings {
    /// Cross-field rules: a key path belongs to key auth only.
    pub fn check_auth_shape(&self) -> Result<(), HostFieldError> {
        match (self.auth, &self.private_key_path) {
            (AuthMethod::PrivateKey | AuthMethod::Certificate, None) => {
                Err(HostFieldError::KeyPath)
            }
            (AuthMethod::Password, Some(_)) => Err(HostFieldError::KeyPath),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_names_trim_spaces_and_refuse_empty_or_control_characters() {
        assert_eq!(
            GroupName::parse(" Production ").expect("group").as_str(),
            "Production"
        );
        for invalid in ["", "   ", "Production\n", "A\u{0}B", &"x".repeat(65)] {
            assert!(GroupName::parse(invalid).is_err());
        }
    }

    #[test]
    fn hostnames_accept_dns_and_ip_literals() {
        for ok in [
            "web-1.example.com",
            "10.0.0.5",
            "[2001:db8::1]",
            "localhost",
            "a",
        ] {
            assert!(Hostname::parse(ok).is_ok(), "{ok}");
        }
        assert_eq!(
            Hostname::parse("Web.Example.COM").expect("ok").as_str(),
            "web.example.com"
        );
    }

    #[test]
    fn hostnames_reject_injection_and_malformed_names() {
        for bad in [
            "",
            " host",
            "host ",
            "ho st",
            "-host",
            "host-",
            "a..b",
            "host;rm -rf /",
            "$(id)",
            "host\n",
            "exa_mple.com",
            &format!("{}.com", "a".repeat(64)),
        ] {
            assert!(Hostname::parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn usernames_are_printable_without_spaces() {
        assert!(Username::parse("deploy").is_ok());
        assert!(Username::parse("").is_err());
        assert!(Username::parse("a b").is_err());
        assert!(Username::parse("root\0").is_err());
        assert!(Username::parse(&"x".repeat(65)).is_err());
    }

    #[test]
    fn timeouts_and_keepalive_are_bounded() {
        assert!(ConnectTimeout::new(0).is_err());
        assert!(ConnectTimeout::new(121).is_err());
        assert_eq!(ConnectTimeout::new(120).expect("max").seconds(), 120);
        assert_eq!(Keepalive::new(0).expect("disabled").seconds(), 0);
        assert!(Keepalive::new(301).is_err());
    }

    #[test]
    fn terminal_type_and_notes_are_constrained() {
        assert!(TerminalType::parse("xterm-256color").is_ok());
        assert!(TerminalType::parse("xterm;ls").is_err());
        assert!(Notes::parse("line one\nline two").is_ok());
        assert!(Notes::parse("bell\u{7}").is_err());
        assert!(Notes::parse(&"n".repeat(4097)).is_err());
    }

    #[test]
    fn auth_shape_requires_key_path_only_for_key_auth() {
        let base = HostSettings {
            label: crate::HostLabel::parse("web").expect("label"),
            hostname: Hostname::parse("web.example.com").expect("host"),
            port: crate::Port::SSH_DEFAULT,
            username: Username::parse("ops").expect("user"),
            auth: AuthMethod::Password,
            private_key_path: None,
            group_id: None,
            connect_timeout: ConnectTimeout::DEFAULT,
            keepalive: Keepalive::DEFAULT,
            terminal_type: TerminalType::parse(TerminalType::DEFAULT).expect("term"),
            notes: None,
        };
        assert!(base.check_auth_shape().is_ok());
        let mut key_without_path = base.clone();
        key_without_path.auth = AuthMethod::PrivateKey;
        assert!(key_without_path.check_auth_shape().is_err());
        let mut password_with_path = base.clone();
        password_with_path.private_key_path = Some(KeyPath::parse("/home/u/.ssh/id").expect("p"));
        assert!(password_with_path.check_auth_shape().is_err());
    }
}
