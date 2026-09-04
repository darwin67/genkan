use std::fmt;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;

use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

const MAGIC: [u8; 4] = *b"GNKA";
const VERSION: u8 = 1;
const HEADER_BYTES: usize = 10;
const MAX_PAYLOAD_BYTES: usize = 16 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024;
pub const MAX_MESSAGE_BYTES: usize = 4 * 1024;
pub const MAX_MESSAGES: u64 = 128;

const READY: u8 = 1;
const PROMPT: u8 = 2;
const INFO: u8 = 3;
const ERROR: u8 = 4;
const SUCCESS: u8 = 5;
const FAILURE: u8 = 6;
const RESPONSE: u8 = 7;
const CANCEL: u8 = 8;
const RETRY: u8 = 9;

#[derive(PartialEq, Eq)]
pub struct Secret(Zeroizing<Vec<u8>>);

impl Secret {
    pub fn new(value: impl Into<Vec<u8>>) -> Result<Self, ProtocolError> {
        let value = value.into();
        if value.len() > MAX_RESPONSE_BYTES || value.contains(&0) {
            return Err(ProtocolError::InvalidPayload);
        }
        Ok(Self(Zeroizing::new(value)))
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}

impl Zeroize for Secret {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Message {
    Ready { uid: u32, username: String },
    Prompt { id: u64, secret: bool, text: String },
    Info(String),
    Error(String),
    Success,
    Failure,
    Response { id: u64, value: Secret },
    Cancel,
    Retry,
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("authentication channel I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("authentication message is malformed or exceeds its bound")]
    InvalidPayload,
    #[error("authentication peer closed the channel")]
    Closed,
}

pub struct Connection {
    stream: UnixStream,
}

impl Connection {
    pub fn new(stream: UnixStream) -> Self {
        Self { stream }
    }

    pub fn send(&mut self, message: &Message) -> Result<(), ProtocolError> {
        let (kind, mut payload) = encode(message)?;
        if payload.len() > MAX_PAYLOAD_BYTES {
            payload.zeroize();
            return Err(ProtocolError::InvalidPayload);
        }
        let mut header = [0_u8; HEADER_BYTES];
        header[..4].copy_from_slice(&MAGIC);
        header[4] = VERSION;
        header[5] = kind;
        header[6..].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        self.stream.write_all(&header)?;
        let result = self.stream.write_all(&payload);
        payload.zeroize();
        result.map_err(Into::into)
    }

    pub fn receive(&mut self) -> Result<Message, ProtocolError> {
        let mut header = [0_u8; HEADER_BYTES];
        read_exact_or_closed(&mut self.stream, &mut header)?;
        if header[..4] != MAGIC || header[4] != VERSION {
            return Err(ProtocolError::InvalidPayload);
        }
        let length = u32::from_be_bytes(header[6..].try_into().expect("fixed header")) as usize;
        if length > MAX_PAYLOAD_BYTES {
            return Err(ProtocolError::InvalidPayload);
        }
        let mut payload = Zeroizing::new(vec![0_u8; length]);
        read_exact_or_closed(&mut self.stream, &mut payload)?;
        decode(header[5], &payload)
    }
}

fn read_exact_or_closed(stream: &mut UnixStream, bytes: &mut [u8]) -> Result<(), ProtocolError> {
    match stream.read_exact(bytes) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Err(ProtocolError::Closed),
        Err(error) => Err(error.into()),
    }
}

fn encode(message: &Message) -> Result<(u8, Vec<u8>), ProtocolError> {
    let encoded = match message {
        Message::Ready { uid, username } => {
            let mut payload = uid.to_be_bytes().to_vec();
            push_text(&mut payload, username, MAX_MESSAGE_BYTES)?;
            (READY, payload)
        }
        Message::Prompt { id, secret, text } => {
            let mut payload = id.to_be_bytes().to_vec();
            payload.push(u8::from(*secret));
            push_text(&mut payload, text, MAX_MESSAGE_BYTES)?;
            (PROMPT, payload)
        }
        Message::Info(text) => (INFO, text_payload(text)?),
        Message::Error(text) => (ERROR, text_payload(text)?),
        Message::Success => (SUCCESS, Vec::new()),
        Message::Failure => (FAILURE, Vec::new()),
        Message::Response { id, value } => {
            let mut payload = id.to_be_bytes().to_vec();
            payload.extend_from_slice(value.expose());
            (RESPONSE, payload)
        }
        Message::Cancel => (CANCEL, Vec::new()),
        Message::Retry => (RETRY, Vec::new()),
    };
    Ok(encoded)
}

fn decode(kind: u8, payload: &[u8]) -> Result<Message, ProtocolError> {
    match kind {
        READY if payload.len() >= 4 => Ok(Message::Ready {
            uid: u32::from_be_bytes(payload[..4].try_into().expect("checked length")),
            username: decode_text(&payload[4..], MAX_MESSAGE_BYTES)?,
        }),
        PROMPT if payload.len() >= 9 && payload[8] <= 1 => Ok(Message::Prompt {
            id: u64::from_be_bytes(payload[..8].try_into().expect("checked length")),
            secret: payload[8] == 1,
            text: decode_text(&payload[9..], MAX_MESSAGE_BYTES)?,
        }),
        INFO => Ok(Message::Info(decode_text(payload, MAX_MESSAGE_BYTES)?)),
        ERROR => Ok(Message::Error(decode_text(payload, MAX_MESSAGE_BYTES)?)),
        SUCCESS if payload.is_empty() => Ok(Message::Success),
        FAILURE if payload.is_empty() => Ok(Message::Failure),
        RESPONSE
            if (8..=8 + MAX_RESPONSE_BYTES).contains(&payload.len())
                && !payload[8..].contains(&0) =>
        {
            Ok(Message::Response {
                id: u64::from_be_bytes(payload[..8].try_into().expect("checked length")),
                value: Secret::new(payload[8..].to_vec())?,
            })
        }
        CANCEL if payload.is_empty() => Ok(Message::Cancel),
        RETRY if payload.is_empty() => Ok(Message::Retry),
        _ => Err(ProtocolError::InvalidPayload),
    }
}

fn text_payload(text: &str) -> Result<Vec<u8>, ProtocolError> {
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(ProtocolError::InvalidPayload);
    }
    Ok(text.as_bytes().to_vec())
}

fn push_text(payload: &mut Vec<u8>, text: &str, maximum: usize) -> Result<(), ProtocolError> {
    if text.len() > maximum {
        return Err(ProtocolError::InvalidPayload);
    }
    payload.extend_from_slice(text.as_bytes());
    Ok(())
}

fn decode_text(payload: &[u8], maximum: usize) -> Result<String, ProtocolError> {
    if payload.len() > maximum || payload.contains(&0) {
        return Err(ProtocolError::InvalidPayload);
    }
    String::from_utf8(payload.to_vec()).map_err(|_| ProtocolError::InvalidPayload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn round_trip(message: Message) -> Message {
        let (left, right) = UnixStream::pair().unwrap();
        let mut sender = Connection::new(left);
        let mut receiver = Connection::new(right);
        sender.send(&message).unwrap();
        receiver.receive().unwrap()
    }

    #[test]
    fn round_trips_every_message_without_exposing_responses_in_debug() {
        let messages = [
            Message::Ready {
                uid: 1000,
                username: "alice".into(),
            },
            Message::Prompt {
                id: 7,
                secret: true,
                text: "Password:".into(),
            },
            Message::Info("Touch your security key".into()),
            Message::Error("Try again".into()),
            Message::Success,
            Message::Failure,
            Message::Response {
                id: 7,
                value: Secret::new(b"sentinel".to_vec()).unwrap(),
            },
            Message::Cancel,
            Message::Retry,
        ];
        for message in messages {
            let received = round_trip(message);
            assert!(!format!("{received:?}").contains("sentinel"));
        }
    }

    #[test]
    fn rejects_oversized_nul_and_malformed_payloads() {
        assert!(Secret::new(vec![b'x'; MAX_RESPONSE_BYTES + 1]).is_err());
        assert!(Secret::new(b"nul\0byte".to_vec()).is_err());

        let (mut sender, receiver) = UnixStream::pair().unwrap();
        sender.write_all(b"GNKA\x01\x02\0\0\x40\x01").unwrap();
        assert!(matches!(
            Connection::new(receiver).receive(),
            Err(ProtocolError::InvalidPayload)
        ));
    }

    #[test]
    fn handles_fragmented_header_and_payload_reads() {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || {
            for byte in b"GNKA\x01\x03\0\0\0\x05hello" {
                sender.write_all(&[*byte]).unwrap();
            }
        });
        assert_eq!(
            Connection::new(receiver).receive().unwrap(),
            Message::Info("hello".into())
        );
        thread.join().unwrap();
    }

    #[test]
    fn rejects_truncated_unknown_and_directionally_invalid_frames() {
        for bytes in [
            b"GNKA\x01\x03\0\0\0\x05hi".as_slice(),
            b"GNKA\x01\xff\0\0\0\0".as_slice(),
            b"NOPE\x01\x05\0\0\0\0".as_slice(),
        ] {
            let (mut sender, receiver) = UnixStream::pair().unwrap();
            sender.write_all(bytes).unwrap();
            drop(sender);
            assert!(Connection::new(receiver).receive().is_err());
        }
    }
}
