//! Backpressure-aware frame dropping.
//!
//! H.264 delta frames depend on their predecessors. When a client cannot keep up the hub must
//! drop frames, but then every later delta frame is undecodable until the next keyframe. The gate
//! therefore drops whole runs: after the first dropped frame it discards deltas until a keyframe
//! arrives, and asks the host for a keyframe. Audio frames stand alone, so a congested client
//! loses only the packets that do not fit. Keyframes and other messages wait for room.

use skylatch_proto::nsl1::{Header, MessageType};

/// Re-request a keyframe after this many frames dropped while waiting for one.
const REREQUEST_AFTER: u32 = 60;

/// What to do with one host-to-client message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Forward,
    /// Forward without dropping, waiting for capacity if needed.
    ForwardReliable,
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
        match header.msg_type {
            MessageType::Frame => {}
            MessageType::AudioFrame if congested => return Verdict::Drop,
            MessageType::AudioFrame => return Verdict::Forward,
            _ => return Verdict::ForwardReliable,
        }
        if header.is_keyframe() {
            self.waiting_keyframe = false;
            self.dropped_since_request = 0;
            return Verdict::ForwardReliable;
        }
        if self.waiting_keyframe {
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
        assert_eq!(g.decide(&frame(1, true), false), Verdict::ForwardReliable);
        assert_eq!(g.decide(&frame(2, false), false), Verdict::Forward);
        assert!(!g.waiting_keyframe());
    }

    #[test]
    fn drops_delta_run_until_keyframe() {
        let mut g = FrameGate::new();
        assert_eq!(g.decide(&frame(1, true), false), Verdict::ForwardReliable);
        assert_eq!(
            g.decide(&frame(2, false), true),
            Verdict::DropAndRequestKeyframe
        );
        assert!(g.waiting_keyframe());
        // Room is back, but a delta frame is still undecodable.
        assert_eq!(g.decide(&frame(3, false), false), Verdict::Drop);
        // Even when congested, the keyframe is forwarded (the relay waits for room).
        assert_eq!(g.decide(&frame(4, true), true), Verdict::ForwardReliable);
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
    fn non_frame_messages_are_reliable() {
        let mut g = FrameGate::new();
        let notice = Message::ClipboardNotice { generation: 1 }.encode().unwrap();
        assert_eq!(g.decide(&notice, false), Verdict::ForwardReliable);
        assert_eq!(g.decide(&notice, true), Verdict::ForwardReliable);
        assert!(!g.waiting_keyframe());
    }

    fn audio(sequence: u32) -> Bytes {
        Message::AudioFrame {
            generation: 1,
            sequence,
            capture_us: 0,
            data: Bytes::from_static(&[0xfc]),
        }
        .encode()
        .unwrap()
    }

    #[test]
    fn audio_frames_drop_alone_under_congestion() {
        let mut g = FrameGate::new();
        let config = Message::AudioConfig {
            generation: 1,
            format: skylatch_proto::nsl1::AudioFormat::opus(),
        }
        .encode()
        .unwrap();
        assert_eq!(g.decide(&config, true), Verdict::ForwardReliable);
        assert_eq!(g.decide(&audio(1), false), Verdict::Forward);
        assert_eq!(g.decide(&audio(2), true), Verdict::Drop);
        assert!(!g.waiting_keyframe());
        assert_eq!(g.decide(&frame(1, false), false), Verdict::Forward);
        assert_eq!(g.decide(&audio(3), false), Verdict::Forward);
    }

    #[test]
    fn audio_frames_pass_while_video_waits_for_a_keyframe() {
        let mut g = FrameGate::new();
        assert_eq!(
            g.decide(&frame(1, false), true),
            Verdict::DropAndRequestKeyframe
        );
        for sequence in 1..=REREQUEST_AFTER * 2 {
            assert_eq!(g.decide(&audio(sequence), false), Verdict::Forward);
        }
        assert!(g.waiting_keyframe());
        assert_eq!(g.decide(&frame(2, false), false), Verdict::Drop);
        assert_eq!(g.decide(&frame(3, true), false), Verdict::ForwardReliable);
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
