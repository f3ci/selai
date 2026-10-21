//! Interactive PTY channel. Bytes are passed through unchanged, so UTF-8 that
//! spans reads is never altered here; decoding is the terminal's job.

use russh::{client::Msg, Channel, ChannelMsg};

use crate::SshError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    /// Output from the remote PTY (stdout and stderr are merged by the PTY).
    Data(Vec<u8>),
    /// The remote shell exited with this status.
    Exit { status: u32 },
    /// The server refused a request (for example, no PTY allowed).
    Refused(String),
    /// The channel is gone. No further events follow.
    Closed,
}

pub struct Shell {
    channel: Option<Channel<Msg>>,
    native: Option<crate::native::NativeShell>,
}

impl Shell {
    pub(crate) fn from_native(native: crate::native::NativeShell) -> Self {
        Self {
            channel: None,
            native: Some(native),
        }
    }
    pub(crate) fn new(channel: Channel<Msg>) -> Self {
        Self {
            channel: Some(channel),
            native: None,
        }
    }

    pub async fn write(&self, bytes: &[u8]) -> Result<(), SshError> {
        // `&[u8]` implements AsyncRead, so the bytes are sent without copying
        // through a String.
        if let Some(native) = &self.native {
            return native.write(bytes);
        }
        self.channel
            .as_ref()
            .ok_or(SshError::ChannelClosed)?
            .data(bytes)
            .await?;
        Ok(())
    }

    pub async fn resize(&self, cols: u32, rows: u32) -> Result<(), SshError> {
        if let Some(native) = &self.native {
            return native.resize(cols, rows);
        }
        self.channel
            .as_ref()
            .ok_or(SshError::ChannelClosed)?
            .window_change(cols, rows, 0, 0)
            .await?;
        Ok(())
    }

    pub async fn next_event(&mut self) -> ShellEvent {
        if let Some(native) = &mut self.native {
            return native.next_event().await;
        }
        let Some(channel) = self.channel.as_mut() else {
            return ShellEvent::Closed;
        };
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => return ShellEvent::Data(data.to_vec()),
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    return ShellEvent::Data(data.to_vec())
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    return ShellEvent::Exit {
                        status: exit_status,
                    }
                }
                Some(ChannelMsg::Failure) => {
                    return ShellEvent::Refused("server refused the terminal request".to_owned())
                }
                // OpenSSH sends EOF before exit-status, so EOF alone does not end the
                // stream; only Close (or the channel dropping) does.
                Some(ChannelMsg::Eof) => continue,
                Some(ChannelMsg::Close) | None => return ShellEvent::Closed,
                Some(_) => continue,
            }
        }
    }

    pub async fn close(&self) -> Result<(), SshError> {
        if let Some(native) = &self.native {
            native.close();
            return Ok(());
        }
        self.channel
            .as_ref()
            .ok_or(SshError::ChannelClosed)?
            .close()
            .await?;
        Ok(())
    }
}
