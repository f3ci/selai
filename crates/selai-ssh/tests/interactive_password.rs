//! Exercise password fallback over a real SSH transport on every platform.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use russh::{server, MethodKind, MethodSet};
use selai_secrets::SecretValue;
use selai_ssh::{connect, Auth, ConnectOptions, Endpoint, PinnedHostKey, SshError};
use std::{
    borrow::Cow,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Clone, Copy)]
enum Challenge {
    Password,
    EmptyRounds,
    Echo,
    Otp,
    Multiple,
    Repeat,
    Endless,
    Direct,
}

struct Handler {
    challenge: Challenge,
    stage: usize,
    attempts: Arc<AtomicUsize>,
    answers: Arc<AtomicUsize>,
}

fn prompt(text: &'static str, echo: bool) -> server::Auth {
    server::Auth::Partial {
        name: Cow::Borrowed("Login"),
        instructions: Cow::Borrowed(""),
        prompts: Cow::Owned(vec![(Cow::Borrowed(text), echo)]),
    }
}

fn empty() -> server::Auth {
    server::Auth::Partial {
        name: Cow::Borrowed("Notice"),
        instructions: Cow::Borrowed("Authorized users only"),
        prompts: Cow::Borrowed(&[]),
    }
}

impl server::Handler for Handler {
    type Error = russh::Error;

    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Ok(
            if matches!(self.challenge, Challenge::Direct)
                && user == "test"
                && password == "correct"
            {
                server::Auth::Accept
            } else {
                server::Auth::reject()
            },
        )
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _: &str,
        response: Option<server::Response<'a>>,
    ) -> Result<server::Auth, Self::Error> {
        let result = if let Some(response) = response {
            let answers: Vec<_> = response.collect();
            self.answers.fetch_add(answers.len(), Ordering::SeqCst);
            if answers.is_empty() {
                if matches!(self.challenge, Challenge::Endless) {
                    empty()
                } else if self.stage == 1 {
                    prompt("Password: ", false)
                } else {
                    server::Auth::Accept
                }
            } else if user != "test" || answers.len() != 1 || answers[0].as_ref() != b"correct" {
                server::Auth::reject()
            } else if matches!(self.challenge, Challenge::Repeat) {
                prompt("Password:", false)
            } else if matches!(self.challenge, Challenge::EmptyRounds) {
                empty()
            } else {
                server::Auth::Accept
            }
        } else {
            match self.challenge {
                Challenge::EmptyRounds | Challenge::Endless => empty(),
                Challenge::Echo => prompt("Password:", true),
                Challenge::Otp => prompt("Verification code:", false),
                Challenge::Multiple => server::Auth::Partial {
                    name: Cow::Borrowed("MFA"),
                    instructions: Cow::Borrowed(""),
                    prompts: Cow::Owned(vec![
                        (Cow::Borrowed("Password:"), false),
                        (Cow::Borrowed("OTP:"), false),
                    ]),
                },
                _ => prompt("Password:", false),
            }
        };
        self.stage += 1;
        Ok(result)
    }
}

async fn attempt(
    challenge: Challenge,
    password: &str,
    pinned: bool,
) -> (Result<(), SshError>, usize, usize) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let key = russh::keys::PrivateKey::from(
        russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[42; 32]),
    );
    let pin = PinnedHostKey::from_openssh_line(&key.public_key().to_openssh().unwrap()).unwrap();
    let config = Arc::new(server::Config {
        keys: vec![key],
        methods: MethodSet::from(&[MethodKind::Password, MethodKind::KeyboardInteractive][..]),
        auth_rejection_time: Duration::ZERO,
        ..Default::default()
    });
    let attempts = Arc::new(AtomicUsize::new(0));
    let answers = Arc::new(AtomicUsize::new(0));
    let handler = Handler {
        challenge,
        stage: 0,
        attempts: attempts.clone(),
        answers: answers.clone(),
    };
    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        if let Ok(session) = server::run_stream(config, stream, handler).await {
            let _ = session.await;
        }
    });
    let result = connect(ConnectOptions {
        endpoint: Endpoint::new("127.0.0.1", port),
        username: "test".into(),
        auth: Auth::Password(SecretValue::new(password)),
        pinned: if pinned { vec![pin] } else { vec![] },
        connect_timeout: Duration::from_secs(5),
        keepalive: None,
    })
    .await;
    let result = match result {
        Ok(session) => {
            session.close().await;
            Ok(())
        }
        Err(error) => Err(error),
    };
    server_task.abort();
    (
        result,
        attempts.load(Ordering::SeqCst),
        answers.load(Ordering::SeqCst),
    )
}

#[tokio::test]
async fn password_only_keyboard_interactive_login_succeeds() {
    for challenge in [Challenge::Password, Challenge::EmptyRounds] {
        let (result, attempts, answers) = attempt(challenge, "correct", true).await;
        result.unwrap();
        assert_eq!(attempts, 1);
        assert_eq!(answers, 1);
    }
}

#[tokio::test]
async fn incorrect_interactive_password_is_rejected() {
    let (result, _, answers) = attempt(Challenge::Password, "incorrect", true).await;
    assert!(matches!(result, Err(SshError::AuthFailed)));
    assert_eq!(answers, 1);
}

#[tokio::test]
async fn unsupported_challenges_receive_no_saved_password() {
    for challenge in [
        Challenge::Echo,
        Challenge::Otp,
        Challenge::Multiple,
        Challenge::Endless,
    ] {
        let (result, _, answers) = attempt(challenge, "correct", true).await;
        assert!(matches!(result, Err(SshError::AuthFailed)), "{result:?}");
        assert_eq!(answers, 0);
    }
}

#[tokio::test]
async fn password_is_never_sent_twice() {
    let (result, _, answers) = attempt(Challenge::Repeat, "correct", true).await;
    assert!(matches!(result, Err(SshError::AuthFailed)));
    assert_eq!(answers, 1);
}

#[tokio::test]
async fn unknown_host_is_rejected_before_authentication() {
    let (result, attempts, answers) = attempt(Challenge::Password, "correct", false).await;
    assert!(matches!(result, Err(SshError::HostKeyUnknown(_))));
    assert_eq!((attempts, answers), (0, 0));
}

#[tokio::test]
async fn direct_password_login_does_not_start_interactive_auth() {
    let (result, attempts, answers) = attempt(Challenge::Direct, "correct", true).await;
    result.unwrap();
    assert_eq!((attempts, answers), (1, 0));
}
