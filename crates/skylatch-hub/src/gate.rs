//! Backpressure-aware frame dropping.
//!
//! H.264 delta frames depend on their predecessors. When a client cannot keep up the hub must
//! drop frames, but then every later delta frame is undecodable until the next keyframe. The gate
//! therefore drops whole runs: after the first dropped frame it discards deltas until a keyframe
//! arrives while the client has room, and asks the host for a keyframe.

use skylatch_proto::nsl1::{Header, MessageType};

/// Re-request a keyframe after this many frames dropped while waiting for one.
const REREQUEST_AFTER: u32 = 60;

/// What to do with one host-to-client message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Forward,
    Drop,
    /// Drop, and ask the host for a keyframe.
    DropAndRequestKeyframe,
}

/// Per-session drop state.
#[derive(Debug, Default)]
pub struct FrameGate {
    waiting_keyframe: bool,
    dropped_since_request: u32,
}

impl FrameGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// True while delta frames are being discarded until a keyframe.
    pub fn waiting_keyframe(&self) -> bool {
        self.waiting_keyframe
    }

    /// Decides the fate of `message`. `congested` means the client's queue is full.
    pub fn decide(&mut self, message: &[u8], congested: bool) -> Verdict {
        let Ok(header) = Header::decode(message) else {
            return Verdict::Drop;
        };
        if header.total_len() != message.len() {
            return Verdict::Drop;
        }
        if header.msg_type != MessageType::Frame {
            // Cursor and clipboard notices are independent; drop them under pressure.
            return if congested {
                Verdict::Drop
            } else {
                Verdict::Forward
            };
        }
        if self.waiting_keyframe {
            if header.is_keyframe() && !congested {
                self.waiting_keyframe = false;
                self.dropped_since_request = 0;
                return Verdict::Forward;
            }
            self.dropped_since_request += 1;
            if self.dropped_since_request >= REREQUEST_AFTER {
                self.dropped_since_request = 0;
                return Verdict::DropAndRequestKeyframe;
            }
            return Verdict::Drop;
        }
        if congested {
            self.waiting_keyframe = true;
            self.dropped_since_request = 0;
            return Verdict::DropAndRequestKeyframe;
        }
        Verdict::Forward
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use skylatch_proto::nsl1::Message;

    fn frame(seq: u32, keyframe: bool) -> Bytes {
        Message::Frame {
            generation: 1,
            sequence: seq,
            keyframe,
            width: 2,
            height: 2,
            capture_us: 0,
            data: Bytes::from_static(&[1, 2, 3]),
        }
        .encode()
        .unwrap()
    }

    #[test]
    fn forwards_when_not_congested() {
        let mut g = FrameGate::new();
        assert_eq!(g.decide(&frame(1, true), false), Verdict::Forward);
        assert_eq!(g.decide(&frame(2, false), false), Verdict::Forward);
        assert!(!g.waiting_keyframe());
    }

    #[test]
    fn drops_delta_run_until_keyframe_with_room() {
        let mut g = FrameGate::new();
        assert_eq!(g.decide(&frame(1, true), false), Verdict::Forward);
        assert_eq!(
            g.decide(&frame(2, false), true),
            Verdict::DropAndRequestKeyframe
        );
        assert!(g.waiting_keyframe());
        // Room is back, but a delta frame is still undecodable.
        assert_eq!(g.decide(&frame(3, false), false), Verdict::Drop);
        // A keyframe while still congested is dropped too.
        assert_eq!(g.decide(&frame(4, true), true), Verdict::Drop);
        assert_eq!(g.decide(&frame(5, true), false), Verdict::Forward);
        assert!(!g.waiting_keyframe());
        assert_eq!(g.decide(&frame(6, false), false), Verdict::Forward);
    }

    #[test]
    fn rerequests_keyframe_periodically() {
        let mut g = FrameGate::new();
        assert_eq!(
            g.decide(&frame(1, false), true),
            Verdict::DropAndRequestKeyframe
        );
        let mut requests = 0;
        for i in 0..REREQUEST_AFTER {
            if g.decide(&frame(2 + i, false), false) == Verdict::DropAndRequestKeyframe {
                requests += 1;
            }
        }
        assert_eq!(requests, 1);
    }

    #[test]
    fn non_frame_messages_drop_only_under_pressure() {
        let mut g = FrameGate::new();
        let notice = Message::ClipboardNotice { generation: 1 }.encode().unwrap();
        assert_eq!(g.decide(&notice, false), Verdict::Forward);
        assert_eq!(g.decide(&notice, true), Verdict::Drop);
        assert!(!g.waiting_keyframe());
    }

    #[test]
    fn malformed_messages_are_dropped() {
        let mut g = FrameGate::new();
        assert_eq!(g.decide(b"nope", false), Verdict::Drop);
        let mut bad = frame(1, true).to_vec();
        bad.push(0);
        assert_eq!(g.decide(&bad, false), Verdict::Drop);
    }
}
