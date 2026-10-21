//! Exercise the actual system OpenSSH client against an isolated SSH fixture.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use russh::server::ChannelOpenHandle;
use russh::{server, Channel, ChannelId};
use selai_ssh::{connect, Auth, ConnectOptions, Endpoint, NativeAuth, PinnedHostKey, SshError};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
struct Handler {
    channels: HashMap<ChannelId, Channel<server::Msg>>,
    auth: Arc<AtomicUsize>,
    key: russh::keys::PublicKey,
    ca: Option<russh::keys::PublicKey>,
    mfa: bool,
    key_verified: bool,
}
impl server::Handler for Handler {
    type Error = russh::Error;
    async fn auth_publickey(
        &mut self,
        user: &str,
        key: &russh::keys::PublicKey,
    ) -> Result<server::Auth, Self::Error> {
        self.auth.fetch_add(1, Ordering::SeqCst);
        if self.ca.is_none() && user == "native-test" && key == &self.key {
            self.key_verified = true;
            if self.mfa {
                return Ok(server::Auth::Reject {
                    proceed_with_methods: Some(russh::MethodSet::from(
                        &[russh::MethodKind::KeyboardInteractive][..],
                    )),
                    partial_success: true,
                });
            }
            return Ok(server::Auth::Accept);
        }
        Ok(server::Auth::reject())
    }
    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _: &str,
        response: Option<server::Response<'a>>,
    ) -> Result<server::Auth, Self::Error> {
        if !self.mfa || !self.key_verified || user != "native-test" {
            return Ok(server::Auth::reject());
        }
        if let Some(mut response) = response {
            return Ok(
                if response
                    .next()
                    .is_some_and(|answer| answer.as_ref() == b"123456")
                {
                    server::Auth::Accept
                } else {
                    server::Auth::reject()
                },
            );
        }
        Ok(server::Auth::Partial {
            name: "MFA".into(),
            instructions: "".into(),
            prompts: vec![("Verification code:".into(), false)].into(),
        })
    }

    async fn auth_openssh_certificate(
        &mut self,
        user: &str,
        certificate: &russh::keys::Certificate,
    ) -> Result<server::Auth, Self::Error> {
        self.auth.fetch_add(1, Ordering::SeqCst);
        Ok(
            if user == "native-test"
                && certificate.valid_principals().iter().any(|p| p == user)
                && self
                    .ca
                    .as_ref()
                    .is_some_and(|ca| ca.key_data() == certificate.signature_key())
            {
                server::Auth::Accept
            } else {
                server::Auth::reject()
            },
        )
    }
    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }
    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(channel, b"native-shell\n".as_slice())?;
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut server::Session,
    ) -> Result<bool, Self::Error> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", *port as u16)).await?;
        *port = listener.local_addr()?.port().into();
        let connected_port = *port;
        let address = address.to_owned();
        let handle = session.handle();
        tokio::spawn(async move {
            if let Ok((mut socket, origin)) = listener.accept().await {
                if let Ok(channel) = handle
                    .channel_open_forwarded_tcpip(
                        address,
                        connected_port,
                        origin.ip().to_string(),
                        origin.port().into(),
                    )
                    .await
                {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                }
            }
        });
        Ok(true)
    }
    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        assert_eq!(data, b"test-command");
        session.channel_success(channel)?;
        session.data(channel, b"native-output\n".as_slice())?;
        session.extended_data(channel, 1, b"native-stderr\n".as_slice())?;
        session.exit_status_request(channel, 7)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<server::Msg>,
        host: &str,
        port: u32,
        _: &str,
        _: u32,
        reply: ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        assert_eq!((host, port), ("echo.test", 1234));
        reply.accept().await;
        tokio::spawn(async move {
            let (mut read, mut write) = tokio::io::split(channel.into_stream());
            let _ = tokio::io::copy(&mut read, &mut write).await;
        });
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        assert_eq!(name, "sftp");
        session.channel_success(channel)?;
        let channel = self.channels.remove(&channel).unwrap();
        tokio::spawn(async move {
            russh_sftp::server::run(channel.into_stream(), Sftp).await;
        });
        Ok(())
    }
}
struct Sftp;
impl russh_sftp::server::Handler for Sftp {
    type Error = russh_sftp::protocol::StatusCode;
    fn unimplemented(&self) -> Self::Error {
        Self::Error::OpUnsupported
    }
}
struct Fixture {
    port: u16,
    pin: PinnedHostKey,
    identity: std::path::PathBuf,
    _dir: tempfile::TempDir,
    stop: CancellationToken,
    auth: Arc<AtomicUsize>,
    broker_address: Option<String>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl Fixture {
    async fn new() -> Self {
        Self::with_certificate(false).await
    }
    async fn with_certificate(certificate: bool) -> Self {
        Self::with_modes(certificate, false).await
    }
    async fn with_modes(certificate: bool, mfa: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let host = russh::keys::PrivateKey::from(
            russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[51; 32]),
        );
        let key = russh::keys::PrivateKey::from(
            russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[52; 32]),
        );
        let pin =
            PinnedHostKey::from_openssh_line(&host.public_key().to_openssh().unwrap()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let identity = dir.path().join("identity");
        std::fs::write(
            &identity,
            key.to_openssh(russh::keys::ssh_key::LineEnding::LF)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&identity, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        #[cfg(windows)]
        {
            let who = std::process::Command::new("whoami.exe")
                .args(["/user", "/fo", "csv", "/nh"])
                .output()
                .unwrap();
            assert!(who.status.success());
            let who = String::from_utf8(who.stdout).unwrap();
            let sid = who.trim().split(',').nth(1).unwrap().trim_matches('"');
            let status = std::process::Command::new("icacls.exe")
                .arg(&identity)
                .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(R)")])
                .output()
                .unwrap();
            assert!(
                status.status.success(),
                "{}",
                String::from_utf8_lossy(&status.stderr)
            );
        }
        let ca = if certificate {
            let ca = russh::keys::PrivateKey::from(
                russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[53; 32]),
            );
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let mut builder = russh::keys::ssh_key::certificate::Builder::new(
                vec![1; 16],
                key.public_key().key_data().clone(),
                now - 60,
                now + 3600,
            )
            .unwrap();
            builder
                .cert_type(russh::keys::ssh_key::certificate::CertType::User)
                .unwrap()
                .valid_principal("native-test")
                .unwrap();
            let certificate = builder.sign(&ca).unwrap();
            std::fs::write(
                dir.path().join("identity-cert.pub"),
                certificate.to_openssh().unwrap(),
            )
            .unwrap();
            Some(ca.public_key().clone())
        } else {
            None
        };
        let config = Arc::new(server::Config {
            keys: vec![host],
            auth_rejection_time: Duration::ZERO,
            ..Default::default()
        });
        let auth = Arc::new(AtomicUsize::new(0));
        let attempts = auth.clone();
        let public = key.public_key().clone();
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {_=stopping.cancelled()=>break,accepted=listener.accept()=>{
                    let Ok((socket,_))=accepted else{break};let config=config.clone();let handler=Handler{channels:HashMap::new(),auth:attempts.clone(),key:public.clone(),ca:ca.clone(),mfa,key_verified:false};let stopping=stopping.clone();
                    tokio::spawn(async move{if let Ok(session)=server::run_stream(config,socket,handler).await{tokio::select!{_=stopping.cancelled()=>{},_=session=>{}}}});
                }}
            }
        });
        let broker_address = if mfa {
            let broker = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = broker.local_addr().unwrap().to_string();
            let stopping = stop.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {_=stopping.cancelled()=>break,accepted=broker.accept()=>{
                        let(mut socket,_)=accepted.unwrap();tokio::spawn(async move{
                            use tokio::io::AsyncBufReadExt;
                            let mut request=String::new();tokio::io::BufReader::new(&mut socket).read_line(&mut request).await.unwrap();
                            assert!(request.contains("native-test-capability"));assert!(request.contains("Verification code:"));
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            let _=socket.write_all(b"{\"answer\":\"123456\"}\n").await;
                        });
                    }}
                }
            });
            Some(address)
        } else {
            None
        };
        Self {
            port,
            pin,
            identity,
            _dir: dir,
            stop,
            auth,
            broker_address,
        }
    }
    fn opts(&self, pinned: bool) -> ConnectOptions {
        ConnectOptions {
            endpoint: Endpoint::new("127.0.0.1", self.port),
            username: "native-test".into(),
            auth: Auth::Native(NativeAuth {
                method: "auto".into(),
                identity: Some(self.identity.clone()),
                helper: std::env::var_os("SELAI_SSH_TEST_HELPER")
                    .map(Into::into)
                    .unwrap_or_else(|| std::env::current_exe().unwrap()),
                environment: if std::env::var_os("SELAI_SSH_TEST_HELPER").is_some() {
                    vec![("SELAI_SSH_AUTH_ADDR".into(), "127.0.0.1:1".into())]
                } else {
                    vec![]
                },
                lease: Arc::new(()),
                cancel: CancellationToken::new(),
            }),
            pinned: if pinned {
                vec![self.pin.clone()]
            } else {
                vec![]
            },
            connect_timeout: Duration::from_secs(5),
            keepalive: None,
        }
    }
}
#[tokio::test]
async fn native_identity_authenticates_and_exec_preserves_both_streams_and_status() {
    let fixture = Fixture::new().await;
    let session = connect(fixture.opts(true)).await.unwrap();
    let output = session
        .exec(
            "test-command",
            Duration::from_secs(10),
            4096,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(output.stdout, b"native-output\n");
    assert_eq!(output.stderr, b"native-stderr\n");
    assert_eq!(output.exit_status, Some(7));
    session.close().await;
}
#[tokio::test]
async fn native_sftp_and_direct_tcpip_use_binary_streams() {
    let fixture = Fixture::new().await;
    let session = connect(fixture.opts(true)).await.unwrap();
    let sftp = tokio::time::timeout(Duration::from_secs(15), session.open_sftp())
        .await
        .unwrap()
        .unwrap();
    sftp.close().await.unwrap();
    let mut stream = tokio::time::timeout(
        Duration::from_secs(15),
        session.direct_tcpip("echo.test", 1234),
    )
    .await
    .unwrap()
    .unwrap();
    stream.write_all(b"\0binary\xff").await.unwrap();
    let mut reply = [0; 8];
    tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&reply, b"\0binary\xff");
    session.close().await;
}
#[tokio::test]
async fn native_unknown_host_is_blocked_before_authentication() {
    let fixture = Fixture::new().await;
    let error = connect(fixture.opts(false)).await.unwrap_err();
    assert!(matches!(error, SshError::HostKeyUnknown(_)));
    assert_eq!(fixture.auth.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn native_changed_host_is_blocked_before_authentication() {
    let fixture = Fixture::new().await;
    let mut opts = fixture.opts(true);
    opts.pinned[0].public_key_base64 = "AAAA".into();
    let error = connect(opts).await.unwrap_err();
    assert!(matches!(error, SshError::HostKeyChanged { .. }));
    assert_eq!(fixture.auth.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn native_user_certificate_authenticates_with_its_private_key() {
    let fixture = Fixture::with_certificate(true).await;
    let mut opts = fixture.opts(true);
    if let Auth::Native(auth) = &mut opts.auth {
        auth.method = "certificate".into();
    }
    let session = connect(opts).await.unwrap();
    assert!(fixture.auth.load(Ordering::SeqCst) > 0);
    session.close().await;
}
#[tokio::test]
async fn native_shell_receives_remote_output_and_exit() {
    let fixture = Fixture::new().await;
    let session = connect(fixture.opts(true)).await.unwrap();
    let mut shell = session.open_shell(80, 24, "xterm-256color").await.unwrap();
    let mut output = Vec::new();
    let mut status = None;
    let mut cursor_queries = 0;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match shell.next_event().await {
                selai_ssh::ShellEvent::Data(data) => {
                    output.extend(data);
                    let queries = String::from_utf8_lossy(&output).matches("\x1b[6n").count();
                    for _ in cursor_queries..queries {
                        shell.write(b"\x1b[1;1R").await.unwrap();
                    }
                    cursor_queries = queries;
                }
                selai_ssh::ShellEvent::Exit { status: code } => status = Some(code),
                selai_ssh::ShellEvent::Closed => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(String::from_utf8_lossy(&output).contains("native-shell"));
    assert_eq!(status, Some(0));
    session.close().await;
}
#[tokio::test]
async fn native_remote_forward_allocates_a_port_and_carries_bytes() {
    let fixture = Fixture::new().await;
    let session = connect(fixture.opts(true)).await.unwrap();
    let (port, mut incoming) = session.remote_forward("127.0.0.1", 0).await.unwrap();
    assert_ne!(port, 0);
    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    client.write_all(b"forward").await.unwrap();
    let mut stream = tokio::time::timeout(Duration::from_secs(10), incoming.recv())
        .await
        .unwrap()
        .unwrap();
    let mut bytes = [0; 7];
    stream.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"forward");
    stream.write_all(b"replied").await.unwrap();
    client.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"replied");
    session.cancel_remote_forward("127.0.0.1", port).await;
    session.close().await;
}

#[tokio::test]
async fn security_key_mode_does_not_authenticate_with_an_ordinary_private_key() {
    let fixture = Fixture::new().await;
    let mut opts = fixture.opts(true);
    if let Auth::Native(auth) = &mut opts.auth {
        auth.method = "security_key".into();
    }
    let error = connect(opts).await.unwrap_err();
    assert!(matches!(error, SshError::NativeAuth(_)));
    assert_eq!(fixture.auth.load(Ordering::SeqCst), 0);
}

#[tokio::test]
#[ignore = "Requires SELAI_SSH_TEST_HELPER pointing to a built Selai executable"]
async fn slow_mfa_uses_the_real_gui_helper_without_consuming_command_timeout() {
    std::env::var_os("SELAI_SSH_TEST_HELPER")
        .expect("Set SELAI_SSH_TEST_HELPER to the built Selai executable");
    let fixture = Fixture::with_modes(false, true).await;
    let mut opts = fixture.opts(true);
    if let Auth::Native(auth) = &mut opts.auth {
        auth.environment = vec![
            (
                "SELAI_SSH_AUTH_ADDR".into(),
                fixture.broker_address.clone().unwrap(),
            ),
            (
                "SELAI_SSH_AUTH_TOKEN".into(),
                "native-test-capability".into(),
            ),
        ];
    }
    let session = connect(opts).await.unwrap();
    let output = session
        .exec(
            "test-command",
            Duration::from_secs(1),
            4096,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(output.end, selai_ssh::ExecEnd::Completed);
    assert_eq!(output.exit_status, Some(7));
    assert_eq!(output.stdout, b"native-output\n");
    assert_eq!(output.stderr, b"native-stderr\n");
    let cancel = CancellationToken::new();
    let stopping = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        stopping.cancel();
    });
    let cancelled = tokio::time::timeout(
        Duration::from_secs(1),
        session.exec("test-command", Duration::from_secs(10), 4096, &cancel),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(cancelled.end, selai_ssh::ExecEnd::Cancelled);
    session.close().await;
}
