//! Incremental `NSL1` parser for byte streams (for example a direct TCP connection).
//!
//! Message-oriented transports such as WebSocket should call [`Message::decode`] directly.

use bytes::{Bytes, BytesMut};
use skylatch_proto::nsl1::{Header, Message, Nsl1Error, HEADER_SIZE};

/// Parser failure. The stream is unusable afterwards and must be reconnected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("protocol error: {0}")]
    Protocol(#[from] Nsl1Error),
}

#[derive(Debug)]
enum State {
    AwaitHeader,
    AwaitPayload(Header),
    Failed,
}

/// Splits a byte stream into [`Message`]s.
#[derive(Debug)]
pub struct FrameParser {
    buf: BytesMut,
    state: State,
}

impl Default for FrameParser {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameParser {
    pub fn new() -> Self {
        Self {
            buf: BytesMut::new(),
            state: State::AwaitHeader,
        }
    }

    /// Bytes buffered but not yet part of a complete message.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Feeds bytes and returns every message completed by them.
    pub fn push(&mut self, data: &[u8]) -> Result<Vec<Message>, ParseError> {
        if matches!(self.state, State::Failed) {
            return Err(Nsl1Error::BadMagic.into());
        }
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            match self.state {
                State::Failed => unreachable!("handled above"),
                State::AwaitHeader => {
                    if self.buf.len() < HEADER_SIZE {
                        break;
                    }
                    match Header::decode(&self.buf) {
                        Ok(header) => self.state = State::AwaitPayload(header),
                        Err(e) => return Err(self.fail(e)),
                    }
                }
                State::AwaitPayload(header) => {
                    if self.buf.len() < header.total_len() {
                        break;
                    }
                    let raw: Bytes = self.buf.split_to(header.total_len()).freeze();
                    self.state = State::AwaitHeader;
                    match Message::decode(&raw) {
                        Ok(msg) => out.push(msg),
                        Err(e) => return Err(self.fail(e)),
                    }
                }
            }
        }
        Ok(out)
    }

    fn fail(&mut self, e: Nsl1Error) -> ParseError {
        self.state = State::Failed;
        self.buf.clear();
        e.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skylatch_proto::nsl1::{Cursor, InputEvent, InputRecord};

    fn sample() -> Vec<Message> {
        vec![
            Message::Frame {
                generation: 1,
                sequence: 1,
                keyframe: true,
                width: 64,
                height: 64,
                capture_us: 5,
                data: Bytes::from_static(&[0, 0, 0, 1, 0x65, 1, 2, 3]),
            },
            Message::ClipboardNotice { generation: 1 },
            Message::Ack {
                generation: 1,
                sequence: 1,
                viewer_wait_us: Some(1200),
            },
            Message::Cursor {
                generation: 1,
                cursor: Cursor::Reset,
            },
            Message::Input {
                generation: 1,
                sequence: 0,
                record: InputRecord::new(InputEvent::Text { scalar: 'é' }),
            },
        ]
    }

    fn wire(msgs: &[Message]) -> Vec<u8> {
        msgs.iter()
            .flat_map(|m| m.encode().unwrap().to_vec())
            .collect()
    }

    #[test]
    fn parses_whole_buffer() {
        let msgs = sample();
        let mut p = FrameParser::new();
        assert_eq!(p.push(&wire(&msgs)).unwrap(), msgs);
        assert_eq!(p.buffered(), 0);
    }

    #[test]
    fn parses_byte_by_byte() {
        let msgs = sample();
        let mut p = FrameParser::new();
        let mut got = Vec::new();
        for b in wire(&msgs) {
            got.extend(p.push(&[b]).unwrap());
        }
        assert_eq!(got, msgs);
    }

    #[test]
    fn holds_partial_message() {
        let msgs = sample();
        let bytes = wire(&msgs[..1]);
        let mut p = FrameParser::new();
        assert!(p.push(&bytes[..10]).unwrap().is_empty());
        assert!(p.push(&bytes[10..bytes.len() - 1]).unwrap().is_empty());
        assert_eq!(p.push(&bytes[bytes.len() - 1..]).unwrap(), msgs[..1]);
    }

    #[test]
    fn fails_closed_on_garbage() {
        let mut p = FrameParser::new();
        let mut bad = vec![0u8; 20];
        bad[..4].copy_from_slice(b"HTTP");
        assert!(p.push(&bad).is_err());
        assert!(p.push(&wire(&sample())).is_err());
    }

    #[test]
    fn fails_on_invalid_payload() {
        // Valid header, clipboard notice claiming a payload.
        let mut bytes = Message::ClipboardNotice { generation: 1 }
            .encode()
            .unwrap()
            .to_vec();
        bytes[19] = 1;
        bytes.push(0);
        let mut p = FrameParser::new();
        assert!(p.push(&bytes).is_err());
    }
}
