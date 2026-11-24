//! `NSL1` binary framing.
//!
//! Every message starts with a 20-byte big-endian header:
//!
//! | Offset | Size | Field |
//! | ---: | ---: | --- |
//! | 0 | 4 | ASCII `NSL1` |
//! | 4 | 1 | version (`1`) |
//! | 5 | 1 | message type |
//! | 6 | 2 | flags (frame bit 0 = keyframe, otherwise zero) |
//! | 8 | 4 | generation (nonzero) |
//! | 12 | 4 | sequence |
//! | 16 | 4 | payload length |
//!
//! One transport message (for example one WebSocket binary message) carries exactly one
//! protocol message.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};

/// Magic bytes `NSL1`.
pub const MAGIC: u32 = 0x4e53_4c31;
/// Protocol version carried in the header.
pub const VERSION: u8 = 1;
/// Header size in bytes.
pub const HEADER_SIZE: usize = 20;
/// Size of one input record.
pub const INPUT_SIZE: usize = 16;
/// Size of the fixed prefix of a frame payload (width, height, capture time).
pub const FRAME_PREFIX_SIZE: usize = 12;
/// Size of the fixed prefix of a cursor payload (width, height, hotspot x, hotspot y).
pub const CURSOR_PREFIX_SIZE: usize = 8;
/// Maximum H.264 access unit size.
pub const MAX_ACCESS_UNIT: usize = 8 * 1024 * 1024;
/// Maximum payload length accepted for any message.
pub const MAX_PAYLOAD: usize = MAX_ACCESS_UNIT + FRAME_PREFIX_SIZE;
/// Maximum cursor edge length in pixels.
pub const MAX_CURSOR_EDGE: u16 = 256;
/// Maximum number of input records in one batch.
pub const MAX_INPUT_BATCH: usize = 256;
/// Maximum UTF-8 JSON size for a monitor list.
pub const MAX_MONITOR_LIST: usize = 64 * 1024;
/// Maximum UTF-8 byte length of a monitor selection ID.
pub const MAX_MONITOR_ID: usize = 256;
/// Maximum UTF-8 byte length of clipboard text.
pub const MAX_CLIPBOARD_DATA: usize = 1024 * 1024;
/// Maximum UTF-8 JSON size of an audio configuration.
pub const MAX_AUDIO_CONFIG: usize = 4096;
/// Maximum audio frame payload: capture time plus one Opus packet.
pub const MAX_AUDIO_FRAME: usize = 16 * 1024;
/// Size of the capture time that prefixes an audio frame payload.
pub const AUDIO_FRAME_PREFIX_SIZE: usize = 8;
/// Largest smooth-wheel magnitude in 1/16 pixel (16384 pixels).
pub const MAX_SMOOTH_WHEEL: i32 = 16384 * 16;
/// Header flag marking a keyframe (frame messages only).
pub const FLAG_KEYFRAME: u16 = 1;
/// WebSocket subprotocol name.
pub const SUBPROTOCOL: &str = "nsl1";

/// Errors produced while decoding or encoding `NSL1` messages.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Nsl1Error {
    #[error("message shorter than the 20-byte header")]
    Truncated,
    #[error("bad magic")]
    BadMagic,
    #[error("unsupported version {0}")]
    BadVersion(u8),
    #[error("unknown message type {0}")]
    UnknownType(u8),
    #[error("invalid flags {0:#06x}")]
    BadFlags(u16),
    #[error("generation must be nonzero")]
    ZeroGeneration,
    #[error("sequence must be zero for this message type")]
    NonzeroSequence,
    #[error("payload length {declared} does not match body length {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("payload of {0} bytes exceeds the limit")]
    PayloadTooLarge(usize),
    #[error("invalid {0} payload")]
    InvalidPayload(&'static str),
    #[error("invalid input record: {0}")]
    InvalidInput(&'static str),
}

/// Message type byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    Frame = 1,
    Ack = 2,
    Input = 3,
    ControlReady = 4,
    InputResponse = 5,
    Cursor = 6,
    ClipboardNotice = 7,
    MonitorList = 17,
    SelectMonitor = 18,
    ClipboardData = 19,
    AudioConfig = 20,
    AudioFrame = 21,
    AudioControl = 22,
}

impl MessageType {
    pub fn from_u8(value: u8) -> Result<Self, Nsl1Error> {
        Ok(match value {
            1 => Self::Frame,
            2 => Self::Ack,
            3 => Self::Input,
            4 => Self::ControlReady,
            5 => Self::InputResponse,
            6 => Self::Cursor,
            7 => Self::ClipboardNotice,
            17 => Self::MonitorList,
            18 => Self::SelectMonitor,
            19 => Self::ClipboardData,
            20 => Self::AudioConfig,
            21 => Self::AudioFrame,
            22 => Self::AudioControl,
            other => return Err(Nsl1Error::UnknownType(other)),
        })
    }

    fn payload_limit(self) -> usize {
        match self {
            Self::MonitorList => MAX_MONITOR_LIST,
            Self::SelectMonitor => MAX_MONITOR_ID,
            Self::ClipboardData => MAX_CLIPBOARD_DATA,
            Self::AudioConfig => MAX_AUDIO_CONFIG,
            Self::AudioFrame => MAX_AUDIO_FRAME,
            Self::AudioControl => 1,
            _ => MAX_PAYLOAD,
        }
    }

    /// SL1 types other than audio frames carry sequence 0.
    fn zero_sequence(self) -> bool {
        matches!(
            self,
            Self::MonitorList
                | Self::SelectMonitor
                | Self::ClipboardData
                | Self::AudioConfig
                | Self::AudioControl
        )
    }

    fn validate_payload_len(self, len: usize) -> Result<(), Nsl1Error> {
        if len > self.payload_limit() {
            return Err(Nsl1Error::PayloadTooLarge(len));
        }
        if self == Self::SelectMonitor && len == 0 {
            return Err(Nsl1Error::InvalidPayload("select monitor"));
        }
        Ok(())
    }
}

/// Decoded 20-byte header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub msg_type: MessageType,
    pub flags: u16,
    pub generation: u32,
    pub sequence: u32,
    pub payload_len: u32,
}

impl Header {
    /// Parses and validates a header from the first 20 bytes of `buf`.
    ///
    /// Trailing bytes are ignored, which lets stream parsers peek at the length.
    pub fn decode(buf: &[u8]) -> Result<Self, Nsl1Error> {
        if buf.len() < HEADER_SIZE {
            return Err(Nsl1Error::Truncated);
        }
        let mut b = &buf[..HEADER_SIZE];
        if b.get_u32() != MAGIC {
            return Err(Nsl1Error::BadMagic);
        }
        let version = b.get_u8();
        if version != VERSION {
            return Err(Nsl1Error::BadVersion(version));
        }
        let msg_type = MessageType::from_u8(b.get_u8())?;
        let flags = b.get_u16();
        let max_flags = if msg_type == MessageType::Frame {
            FLAG_KEYFRAME
        } else {
            0
        };
        if flags & !max_flags != 0 {
            return Err(Nsl1Error::BadFlags(flags));
        }
        let generation = b.get_u32();
        if generation == 0 {
            return Err(Nsl1Error::ZeroGeneration);
        }
        let sequence = b.get_u32();
        let payload_len = b.get_u32();
        if msg_type.zero_sequence() && sequence != 0 {
            return Err(Nsl1Error::NonzeroSequence);
        }
        msg_type.validate_payload_len(payload_len as usize)?;
        Ok(Self {
            msg_type,
            flags,
            generation,
            sequence,
            payload_len,
        })
    }

    /// Serialises the header.
    pub fn encode(&self) -> [u8; HEADER_SIZE] {
        let mut out = [0u8; HEADER_SIZE];
        let mut b = &mut out[..];
        b.put_u32(MAGIC);
        b.put_u8(VERSION);
        b.put_u8(self.msg_type as u8);
        b.put_u16(self.flags);
        b.put_u32(self.generation);
        b.put_u32(self.sequence);
        b.put_u32(self.payload_len);
        out
    }

    /// Total message length (header plus payload).
    pub fn total_len(&self) -> usize {
        HEADER_SIZE + self.payload_len as usize
    }

    /// True if this is a frame message flagged as a keyframe.
    pub fn is_keyframe(&self) -> bool {
        self.msg_type == MessageType::Frame && self.flags & FLAG_KEYFRAME != 0
    }
}

/// Wheel axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelAxis {
    Vertical,
    Horizontal,
}

impl WheelAxis {
    fn code(self) -> u16 {
        match self {
            Self::Vertical => 0,
            Self::Horizontal => 1,
        }
    }

    fn from_code(code: u16) -> Result<Self, Nsl1Error> {
        match code {
            0 => Ok(Self::Vertical),
            1 => Ok(Self::Horizontal),
            _ => Err(Nsl1Error::InvalidInput("invalid wheel axis")),
        }
    }
}

/// Input event kind. Coordinates are 0..=65535 across the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// HID usage 0..=231.
    KeyDown {
        usage: u16,
    },
    KeyUp {
        usage: u16,
    },
    PointerMove {
        x: u16,
        y: u16,
    },
    /// Button 1..=3.
    ButtonDown {
        button: u16,
        x: u16,
        y: u16,
    },
    ButtonUp {
        button: u16,
        x: u16,
        y: u16,
    },
    /// Delta is a multiple of 120 within -1200..=1200.
    Wheel {
        axis: WheelAxis,
        delta: i32,
        x: u16,
        y: u16,
    },
    ReleaseAll,
    /// One Unicode scalar value (type 8).
    Text {
        scalar: char,
    },
    /// Type 9. Delta is in 1/16 pixel, nonzero within +/-`MAX_SMOOTH_WHEEL`.
    SmoothWheel {
        axis: WheelAxis,
        delta: i32,
        x: u16,
        y: u16,
    },
}

/// A 16-byte input record: event plus button and modifier masks.
///
/// | Offset | Size | Field |
/// | ---: | ---: | --- |
/// | 0 | 1 | type |
/// | 1 | 1 | reserved (zero) |
/// | 2 | 2 | code (key, button or wheel axis) |
/// | 4 | 4 | value (signed wheel delta, smooth-wheel 1/16 pixels or Unicode scalar) |
/// | 8, 10 | 2+2 | x, y |
/// | 12 | 2 | button mask (low 5 bits) |
/// | 14 | 2 | modifier mask (low 8 bits) |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputRecord {
    pub event: InputEvent,
    pub buttons: u16,
    pub modifiers: u16,
}

impl InputRecord {
    pub fn new(event: InputEvent) -> Self {
        Self {
            event,
            buttons: 0,
            modifiers: 0,
        }
    }

    pub fn with_masks(mut self, buttons: u16, modifiers: u16) -> Self {
        self.buttons = buttons;
        self.modifiers = modifiers;
        self
    }

    /// Encodes the record, validating field ranges.
    pub fn encode(&self) -> Result<[u8; INPUT_SIZE], Nsl1Error> {
        if self.buttons > 0x1f {
            return Err(Nsl1Error::InvalidInput("button mask exceeds 5 bits"));
        }
        if self.modifiers > 0xff {
            return Err(Nsl1Error::InvalidInput("modifier mask exceeds 8 bits"));
        }
        let (kind, code, value, x, y): (u8, u16, i32, u16, u16) = match self.event {
            InputEvent::KeyDown { usage } | InputEvent::KeyUp { usage } if usage > 231 => {
                return Err(Nsl1Error::InvalidInput("key usage above 231"))
            }
            InputEvent::KeyDown { usage } => (1, usage, 0, 0, 0),
            InputEvent::KeyUp { usage } => (2, usage, 0, 0, 0),
            InputEvent::PointerMove { x, y } => (3, 0, 0, x, y),
            InputEvent::ButtonDown { button, x, y } => (4, check_button(button)?, 0, x, y),
            InputEvent::ButtonUp { button, x, y } => (5, check_button(button)?, 0, x, y),
            InputEvent::Wheel { axis, delta, x, y } => {
                if !(-1200..=1200).contains(&delta) || delta % 120 != 0 {
                    return Err(Nsl1Error::InvalidInput(
                        "wheel delta must be a multiple of 120 within +/-1200",
                    ));
                }
                (6, axis.code(), delta, x, y)
            }
            InputEvent::ReleaseAll => (7, 0, 0, 0, 0),
            InputEvent::Text { scalar } => (8, 0, scalar as i32, 0, 0),
            InputEvent::SmoothWheel { axis, delta, x, y } => {
                check_smooth(delta)?;
                (9, axis.code(), delta, x, y)
            }
        };
        if matches!(self.event, InputEvent::ReleaseAll | InputEvent::Text { .. })
            && (self.buttons != 0 || self.modifiers != 0)
        {
            return Err(Nsl1Error::InvalidInput(
                "release and text events require zero masks",
            ));
        }
        let mut out = [0u8; INPUT_SIZE];
        let mut b = &mut out[..];
        b.put_u8(kind);
        b.put_u8(0);
        b.put_u16(code);
        b.put_i32(value);
        b.put_u16(x);
        b.put_u16(y);
        b.put_u16(self.buttons);
        b.put_u16(self.modifiers);
        Ok(out)
    }

    /// Decodes and validates one 16-byte record.
    pub fn decode(buf: &[u8]) -> Result<Self, Nsl1Error> {
        if buf.len() != INPUT_SIZE {
            return Err(Nsl1Error::InvalidInput("record must be 16 bytes"));
        }
        let mut b = buf;
        let kind = b.get_u8();
        if b.get_u8() != 0 {
            return Err(Nsl1Error::InvalidInput("reserved byte must be zero"));
        }
        let code = b.get_u16();
        let value = b.get_i32();
        let x = b.get_u16();
        let y = b.get_u16();
        let buttons = b.get_u16();
        let modifiers = b.get_u16();
        if buttons > 0x1f || modifiers > 0xff {
            return Err(Nsl1Error::InvalidInput("mask out of range"));
        }
        let no_value = |ok: bool| {
            if ok {
                Ok(())
            } else {
                Err(Nsl1Error::InvalidInput("unused field must be zero"))
            }
        };
        let event = match kind {
            1 | 2 => {
                if code > 231 {
                    return Err(Nsl1Error::InvalidInput("key usage above 231"));
                }
                no_value(value == 0 && x == 0 && y == 0)?;
                if kind == 1 {
                    InputEvent::KeyDown { usage: code }
                } else {
                    InputEvent::KeyUp { usage: code }
                }
            }
            3 => {
                no_value(code == 0 && value == 0)?;
                InputEvent::PointerMove { x, y }
            }
            4 | 5 => {
                check_button(code)?;
                no_value(value == 0)?;
                if kind == 4 {
                    InputEvent::ButtonDown { button: code, x, y }
                } else {
                    InputEvent::ButtonUp { button: code, x, y }
                }
            }
            6 => {
                let axis = WheelAxis::from_code(code)?;
                if !(-1200..=1200).contains(&value) || value % 120 != 0 {
                    return Err(Nsl1Error::InvalidInput(
                        "wheel delta must be a multiple of 120 within +/-1200",
                    ));
                }
                InputEvent::Wheel {
                    axis,
                    delta: value,
                    x,
                    y,
                }
            }
            7 => {
                no_value(code == 0 && value == 0 && x == 0 && y == 0)?;
                InputEvent::ReleaseAll
            }
            8 => {
                no_value(code == 0 && x == 0 && y == 0)?;
                let scalar = u32::try_from(value)
                    .ok()
                    .and_then(char::from_u32)
                    .filter(|c| *c != '\0')
                    .ok_or(Nsl1Error::InvalidInput("invalid Unicode scalar"))?;
                InputEvent::Text { scalar }
            }
            9 => InputEvent::SmoothWheel {
                axis: WheelAxis::from_code(code)?,
                delta: check_smooth(value)?,
                x,
                y,
            },
            _ => return Err(Nsl1Error::InvalidInput("unknown input type")),
        };
        if matches!(event, InputEvent::ReleaseAll | InputEvent::Text { .. })
            && (buttons != 0 || modifiers != 0)
        {
            return Err(Nsl1Error::InvalidInput(
                "release and text events require zero masks",
            ));
        }
        Ok(Self {
            event,
            buttons,
            modifiers,
        })
    }
}

fn check_smooth(delta: i32) -> Result<i32, Nsl1Error> {
    if delta != 0 && (-MAX_SMOOTH_WHEEL..=MAX_SMOOTH_WHEEL).contains(&delta) {
        Ok(delta)
    } else {
        Err(Nsl1Error::InvalidInput(
            "smooth wheel delta must be nonzero within +/-262144",
        ))
    }
}

fn check_button(button: u16) -> Result<u16, Nsl1Error> {
    if (1..=3).contains(&button) {
        Ok(button)
    } else {
        Err(Nsl1Error::InvalidInput("button must be 1..=3"))
    }
}

/// Status byte of an input response (type 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InputStatus {
    Accepted = 0,
    Invalid = 1,
    Unsupported = 2,
}

/// A decoded `NSL1` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Type 1: one H.264 Annex B access unit.
    Frame {
        generation: u32,
        sequence: u32,
        keyframe: bool,
        width: u16,
        height: u16,
        /// Capture time in microseconds.
        capture_us: u64,
        data: Bytes,
    },
    /// Type 2: viewer acknowledgement. `viewer_wait_us` is `None` for the empty payload.
    /// Sequence 0 with an empty payload asks whether the host accepts smooth-wheel records.
    Ack {
        generation: u32,
        sequence: u32,
        viewer_wait_us: Option<u32>,
    },
    /// Type 3: one input record. `sequence` is zero on a stream, a request id on a control channel.
    Input {
        generation: u32,
        sequence: u32,
        record: InputRecord,
    },
    /// Type 4: desktop size announced on a control channel.
    ControlReady {
        generation: u32,
        width: u16,
        height: u16,
    },
    /// Type 5: response to an input request, or with sequence 0 on a stream, to the smooth-wheel query.
    InputResponse {
        generation: u32,
        sequence: u32,
        status: InputStatus,
    },
    /// Type 6: cursor image, straight-alpha RGBA. `Reset` restores the local arrow.
    Cursor { generation: u32, cursor: Cursor },
    /// Type 7: the desktop clipboard changed.
    ClipboardNotice { generation: u32 },
    /// Type 17: the available displays and current selection.
    MonitorList {
        generation: u32,
        monitors: Vec<MonitorInfo>,
    },
    /// Type 18: select a display by its opaque ID.
    SelectMonitor { generation: u32, id: String },
    /// Type 19: bidirectional text clipboard data.
    ClipboardData { generation: u32, text: String },
    /// Type 20: the audio format, sent before the first audio frame.
    AudioConfig {
        generation: u32,
        format: AudioFormat,
    },
    /// Type 21: one encoded audio packet. Sequence starts at 1 and grows by one per packet.
    AudioFrame {
        generation: u32,
        sequence: u32,
        /// Capture time in microseconds.
        capture_us: u64,
        data: Bytes,
    },
    /// Type 22: the viewer asks the host to start or stop audio.
    AudioControl { generation: u32, enabled: bool },
}

/// The audio stream format announced by an audio configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u8,
    pub frame_ms: u16,
}

impl AudioFormat {
    /// 48 kHz stereo Opus in 20 ms packets, the format hosts send.
    pub fn opus() -> Self {
        Self {
            codec: "opus".into(),
            sample_rate: 48_000,
            channels: 2,
            frame_ms: 20,
        }
    }
}

/// One display as described by the host in a monitor list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub id: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u16,
    pub height: u16,
    pub primary: bool,
    pub active: bool,
}

/// Cursor payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cursor {
    /// Eight zero bytes: restore the local arrow.
    Reset,
    Image {
        width: u16,
        height: u16,
        hot_x: u16,
        hot_y: u16,
        /// `width * height * 4` bytes of straight-alpha RGBA, row by row.
        rgba: Bytes,
    },
}

impl Message {
    pub fn msg_type(&self) -> MessageType {
        match self {
            Self::Frame { .. } => MessageType::Frame,
            Self::Ack { .. } => MessageType::Ack,
            Self::Input { .. } => MessageType::Input,
            Self::ControlReady { .. } => MessageType::ControlReady,
            Self::InputResponse { .. } => MessageType::InputResponse,
            Self::Cursor { .. } => MessageType::Cursor,
            Self::ClipboardNotice { .. } => MessageType::ClipboardNotice,
            Self::MonitorList { .. } => MessageType::MonitorList,
            Self::SelectMonitor { .. } => MessageType::SelectMonitor,
            Self::ClipboardData { .. } => MessageType::ClipboardData,
            Self::AudioConfig { .. } => MessageType::AudioConfig,
            Self::AudioFrame { .. } => MessageType::AudioFrame,
            Self::AudioControl { .. } => MessageType::AudioControl,
        }
    }

    /// Encodes the message into one contiguous buffer.
    pub fn encode(&self) -> Result<Bytes, Nsl1Error> {
        let (flags, generation, sequence, payload): (u16, u32, u32, Bytes) = match self {
            Self::Frame {
                generation,
                sequence,
                keyframe,
                width,
                height,
                capture_us,
                data,
            } => {
                if data.len() > MAX_ACCESS_UNIT {
                    return Err(Nsl1Error::PayloadTooLarge(data.len()));
                }
                let mut p = BytesMut::with_capacity(FRAME_PREFIX_SIZE + data.len());
                p.put_u16(*width);
                p.put_u16(*height);
                p.put_u64(*capture_us);
                p.extend_from_slice(data);
                (
                    if *keyframe { FLAG_KEYFRAME } else { 0 },
                    *generation,
                    *sequence,
                    p.freeze(),
                )
            }
            Self::Ack {
                generation,
                sequence,
                viewer_wait_us,
            } => {
                let mut p = BytesMut::new();
                if let Some(wait) = viewer_wait_us {
                    p.put_u32(*wait);
                }
                (0, *generation, *sequence, p.freeze())
            }
            Self::Input {
                generation,
                sequence,
                record,
            } => (
                0,
                *generation,
                *sequence,
                Bytes::copy_from_slice(&record.encode()?),
            ),
            Self::ControlReady {
                generation,
                width,
                height,
            } => {
                let mut p = BytesMut::with_capacity(4);
                p.put_u16(*width);
                p.put_u16(*height);
                (0, *generation, 0, p.freeze())
            }
            Self::InputResponse {
                generation,
                sequence,
                status,
            } => (
                0,
                *generation,
                *sequence,
                Bytes::copy_from_slice(&[*status as u8]),
            ),
            Self::Cursor { generation, cursor } => {
                let mut p = BytesMut::new();
                match cursor {
                    Cursor::Reset => p.put_bytes(0, CURSOR_PREFIX_SIZE),
                    Cursor::Image {
                        width,
                        height,
                        hot_x,
                        hot_y,
                        rgba,
                    } => {
                        validate_cursor(*width, *height, *hot_x, *hot_y, rgba.len())?;
                        p.put_u16(*width);
                        p.put_u16(*height);
                        p.put_u16(*hot_x);
                        p.put_u16(*hot_y);
                        p.extend_from_slice(rgba);
                    }
                }
                (0, *generation, 0, p.freeze())
            }
            Self::ClipboardNotice { generation } => (0, *generation, 0, Bytes::new()),
            Self::MonitorList {
                generation,
                monitors,
            } => {
                let payload = serde_json::to_vec(monitors)
                    .map_err(|_| Nsl1Error::InvalidPayload("monitor list"))?;
                MessageType::MonitorList.validate_payload_len(payload.len())?;
                (0, *generation, 0, Bytes::from(payload))
            }
            Self::SelectMonitor { generation, id } => {
                MessageType::SelectMonitor.validate_payload_len(id.len())?;
                (0, *generation, 0, Bytes::copy_from_slice(id.as_bytes()))
            }
            Self::ClipboardData { generation, text } => {
                MessageType::ClipboardData.validate_payload_len(text.len())?;
                (0, *generation, 0, Bytes::copy_from_slice(text.as_bytes()))
            }
            Self::AudioConfig { generation, format } => {
                let payload = serde_json::to_vec(format)
                    .map_err(|_| Nsl1Error::InvalidPayload("audio config"))?;
                MessageType::AudioConfig.validate_payload_len(payload.len())?;
                (0, *generation, 0, Bytes::from(payload))
            }
            Self::AudioFrame {
                generation,
                sequence,
                capture_us,
                data,
            } => {
                if *sequence == 0 || data.is_empty() {
                    return Err(Nsl1Error::InvalidPayload("audio frame"));
                }
                MessageType::AudioFrame
                    .validate_payload_len(AUDIO_FRAME_PREFIX_SIZE + data.len())?;
                let mut p = BytesMut::with_capacity(AUDIO_FRAME_PREFIX_SIZE + data.len());
                p.put_u64(*capture_us);
                p.extend_from_slice(data);
                (0, *generation, *sequence, p.freeze())
            }
            Self::AudioControl {
                generation,
                enabled,
            } => (
                0,
                *generation,
                0,
                Bytes::copy_from_slice(&[u8::from(*enabled)]),
            ),
        };
        if generation == 0 {
            return Err(Nsl1Error::ZeroGeneration);
        }
        let header = Header {
            msg_type: self.msg_type(),
            flags,
            generation,
            sequence,
            payload_len: payload.len() as u32,
        };
        let mut out = BytesMut::with_capacity(HEADER_SIZE + payload.len());
        out.extend_from_slice(&header.encode());
        out.extend_from_slice(&payload);
        Ok(out.freeze())
    }

    /// Decodes exactly one message. The payload of frames and cursors is a zero-copy slice of `buf`.
    pub fn decode(buf: &Bytes) -> Result<Self, Nsl1Error> {
        let header = Header::decode(buf)?;
        let body_len = buf.len() - HEADER_SIZE;
        if header.payload_len as usize != body_len {
            return Err(Nsl1Error::LengthMismatch {
                declared: header.payload_len as usize,
                actual: body_len,
            });
        }
        let payload = buf.slice(HEADER_SIZE..);
        let Header {
            generation,
            sequence,
            ..
        } = header;
        match header.msg_type {
            MessageType::Frame => {
                if payload.len() < FRAME_PREFIX_SIZE {
                    return Err(Nsl1Error::InvalidPayload("frame"));
                }
                let mut p = &payload[..FRAME_PREFIX_SIZE];
                let width = p.get_u16();
                let height = p.get_u16();
                let capture_us = p.get_u64();
                Ok(Self::Frame {
                    generation,
                    sequence,
                    keyframe: header.is_keyframe(),
                    width,
                    height,
                    capture_us,
                    data: payload.slice(FRAME_PREFIX_SIZE..),
                })
            }
            MessageType::Ack => {
                let viewer_wait_us = match payload.len() {
                    0 => None,
                    4 => Some((&payload[..]).get_u32()),
                    _ => return Err(Nsl1Error::InvalidPayload("ack")),
                };
                Ok(Self::Ack {
                    generation,
                    sequence,
                    viewer_wait_us,
                })
            }
            MessageType::Input => Ok(Self::Input {
                generation,
                sequence,
                record: InputRecord::decode(&payload)?,
            }),
            MessageType::ControlReady => {
                if payload.len() != 4 || sequence != 0 {
                    return Err(Nsl1Error::InvalidPayload("control-ready"));
                }
                let mut p = &payload[..];
                Ok(Self::ControlReady {
                    generation,
                    width: p.get_u16(),
                    height: p.get_u16(),
                })
            }
            MessageType::InputResponse => {
                if payload.len() != 1 {
                    return Err(Nsl1Error::InvalidPayload("input-response"));
                }
                let status = match payload[0] {
                    0 => InputStatus::Accepted,
                    1 => InputStatus::Invalid,
                    2 => InputStatus::Unsupported,
                    _ => return Err(Nsl1Error::InvalidPayload("input-response")),
                };
                Ok(Self::InputResponse {
                    generation,
                    sequence,
                    status,
                })
            }
            MessageType::Cursor => {
                if payload.len() < CURSOR_PREFIX_SIZE || sequence != 0 {
                    return Err(Nsl1Error::InvalidPayload("cursor"));
                }
                let mut p = &payload[..CURSOR_PREFIX_SIZE];
                let (width, height, hot_x, hot_y) =
                    (p.get_u16(), p.get_u16(), p.get_u16(), p.get_u16());
                let pixels = payload.len() - CURSOR_PREFIX_SIZE;
                if width == 0 && height == 0 && hot_x == 0 && hot_y == 0 && pixels == 0 {
                    return Ok(Self::Cursor {
                        generation,
                        cursor: Cursor::Reset,
                    });
                }
                validate_cursor(width, height, hot_x, hot_y, pixels)?;
                Ok(Self::Cursor {
                    generation,
                    cursor: Cursor::Image {
                        width,
                        height,
                        hot_x,
                        hot_y,
                        rgba: payload.slice(CURSOR_PREFIX_SIZE..),
                    },
                })
            }
            MessageType::ClipboardNotice => {
                if !payload.is_empty() || sequence != 0 {
                    return Err(Nsl1Error::InvalidPayload("clipboard notice"));
                }
                Ok(Self::ClipboardNotice { generation })
            }
            MessageType::MonitorList => {
                let invalid = |_| Nsl1Error::InvalidPayload("monitor list");
                let json = std::str::from_utf8(&payload)
                    .map_err(|_| Nsl1Error::InvalidPayload("monitor list"))?;
                // The wire schema calls for JSON objects, not positional arrays.
                let entries =
                    serde_json::from_str::<Vec<serde_json::Map<String, serde_json::Value>>>(json)
                        .map_err(invalid)?;
                let monitors = entries
                    .into_iter()
                    .map(|entry| serde_json::from_value(serde_json::Value::Object(entry)))
                    .collect::<Result<_, _>>()
                    .map_err(invalid)?;
                Ok(Self::MonitorList {
                    generation,
                    monitors,
                })
            }
            MessageType::SelectMonitor => Ok(Self::SelectMonitor {
                generation,
                id: String::from_utf8(payload.to_vec())
                    .map_err(|_| Nsl1Error::InvalidPayload("select monitor"))?,
            }),
            MessageType::ClipboardData => Ok(Self::ClipboardData {
                generation,
                text: String::from_utf8(payload.to_vec())
                    .map_err(|_| Nsl1Error::InvalidPayload("clipboard data"))?,
            }),
            MessageType::AudioConfig => {
                let invalid = |_| Nsl1Error::InvalidPayload("audio config");
                let entry =
                    serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&payload)
                        .map_err(invalid)?;
                Ok(Self::AudioConfig {
                    generation,
                    format: serde_json::from_value(serde_json::Value::Object(entry))
                        .map_err(invalid)?,
                })
            }
            MessageType::AudioFrame => {
                if payload.len() <= AUDIO_FRAME_PREFIX_SIZE || sequence == 0 {
                    return Err(Nsl1Error::InvalidPayload("audio frame"));
                }
                Ok(Self::AudioFrame {
                    generation,
                    sequence,
                    capture_us: (&payload[..AUDIO_FRAME_PREFIX_SIZE]).get_u64(),
                    data: payload.slice(AUDIO_FRAME_PREFIX_SIZE..),
                })
            }
            MessageType::AudioControl => match payload[..] {
                [flag @ (0 | 1)] => Ok(Self::AudioControl {
                    generation,
                    enabled: flag == 1,
                }),
                _ => Err(Nsl1Error::InvalidPayload("audio control")),
            },
        }
    }
}

fn validate_cursor(
    width: u16,
    height: u16,
    hot_x: u16,
    hot_y: u16,
    pixel_bytes: usize,
) -> Result<(), Nsl1Error> {
    let ok = width > 0
        && height > 0
        && width <= MAX_CURSOR_EDGE
        && height <= MAX_CURSOR_EDGE
        && hot_x < width
        && hot_y < height
        && pixel_bytes == width as usize * height as usize * 4;
    if ok {
        Ok(())
    } else {
        Err(Nsl1Error::InvalidPayload("cursor"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(msg: Message) -> Bytes {
        let wire = msg.encode().expect("encode");
        assert_eq!(Message::decode(&wire).expect("decode"), msg);
        wire
    }

    #[test]
    fn header_layout_is_exact() {
        let h = Header {
            msg_type: MessageType::Frame,
            flags: 1,
            generation: 0x0102_0304,
            sequence: 0x0a0b_0c0d,
            payload_len: 0x0000_0010,
        };
        let bytes = h.encode();
        assert_eq!(
            bytes,
            [
                b'N', b'S', b'L', b'1', 1, 1, 0, 1, 1, 2, 3, 4, 0x0a, 0x0b, 0x0c, 0x0d, 0, 0, 0,
                0x10
            ]
        );
        assert_eq!(Header::decode(&bytes).unwrap(), h);
        assert_eq!(h.total_len(), 36);
        assert!(h.is_keyframe());
    }

    #[test]
    fn header_rejects_malformed() {
        let good = Header {
            msg_type: MessageType::ClipboardNotice,
            flags: 0,
            generation: 1,
            sequence: 0,
            payload_len: 0,
        }
        .encode();
        assert_eq!(Header::decode(&good[..19]), Err(Nsl1Error::Truncated));
        let mut b = good;
        b[0] = b'X';
        assert_eq!(Header::decode(&b), Err(Nsl1Error::BadMagic));
        let mut b = good;
        b[4] = 2;
        assert_eq!(Header::decode(&b), Err(Nsl1Error::BadVersion(2)));
        for t in [0u8, 8, 23, 255] {
            let mut b = good;
            b[5] = t;
            assert_eq!(Header::decode(&b), Err(Nsl1Error::UnknownType(t)));
        }
        let mut b = good;
        b[7] = 1; // keyframe flag on a non-frame
        assert_eq!(Header::decode(&b), Err(Nsl1Error::BadFlags(1)));
        let mut b = good;
        b[5] = 1;
        b[6] = 0x80; // unknown flag on a frame
        assert_eq!(Header::decode(&b), Err(Nsl1Error::BadFlags(0x8000)));
        let mut b = good;
        b[8..12].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(Header::decode(&b), Err(Nsl1Error::ZeroGeneration));
        let mut b = good;
        b[16..20].copy_from_slice(&(MAX_PAYLOAD as u32 + 1).to_be_bytes());
        assert_eq!(
            Header::decode(&b),
            Err(Nsl1Error::PayloadTooLarge(MAX_PAYLOAD + 1))
        );
    }

    #[test]
    fn frame_roundtrip_and_layout() {
        let wire = roundtrip(Message::Frame {
            generation: 7,
            sequence: 1,
            keyframe: true,
            width: 5120,
            height: 2880,
            capture_us: 0x0102_0304_0506_0708,
            data: Bytes::from_static(&[0, 0, 0, 1, 0x67, 0x42]),
        });
        assert_eq!(wire.len(), HEADER_SIZE + 12 + 6);
        assert_eq!(&wire[5..8], &[1, 0, 1]);
        assert_eq!(&wire[20..24], &[0x14, 0x00, 0x0b, 0x40]); // 5120 x 2880 big-endian
        assert_eq!(&wire[24..32], &[1, 2, 3, 4, 5, 6, 7, 8]);
        roundtrip(Message::Frame {
            generation: 7,
            sequence: 2,
            keyframe: false,
            width: 1,
            height: 1,
            capture_us: 0,
            data: Bytes::new(),
        });
    }

    #[test]
    fn frame_decode_is_zero_copy() {
        let wire = Message::Frame {
            generation: 1,
            sequence: 1,
            keyframe: false,
            width: 2,
            height: 2,
            capture_us: 9,
            data: Bytes::from_static(b"abcdef"),
        }
        .encode()
        .unwrap();
        let Message::Frame { data, .. } = Message::decode(&wire).unwrap() else {
            panic!("not a frame");
        };
        assert_eq!(
            data.as_ptr(),
            wire[HEADER_SIZE + FRAME_PREFIX_SIZE..].as_ptr()
        );
    }

    #[test]
    fn frame_limits() {
        let too_big = Message::Frame {
            generation: 1,
            sequence: 1,
            keyframe: true,
            width: 1,
            height: 1,
            capture_us: 0,
            data: Bytes::from(vec![0u8; MAX_ACCESS_UNIT + 1]),
        };
        assert!(matches!(
            too_big.encode(),
            Err(Nsl1Error::PayloadTooLarge(_))
        ));
        let at_limit = Message::Frame {
            generation: 1,
            sequence: 1,
            keyframe: true,
            width: 1,
            height: 1,
            capture_us: 0,
            data: Bytes::from(vec![0u8; MAX_ACCESS_UNIT]),
        };
        assert_eq!(at_limit.encode().unwrap().len(), 8_388_640);
        // Payload shorter than the fixed frame prefix.
        let mut raw = BytesMut::new();
        raw.extend_from_slice(
            &Header {
                msg_type: MessageType::Frame,
                flags: 0,
                generation: 1,
                sequence: 1,
                payload_len: 4,
            }
            .encode(),
        );
        raw.extend_from_slice(&[0; 4]);
        assert_eq!(
            Message::decode(&raw.freeze()),
            Err(Nsl1Error::InvalidPayload("frame"))
        );
    }

    #[test]
    fn ack_with_and_without_wait() {
        let wire = roundtrip(Message::Ack {
            generation: 3,
            sequence: 99,
            viewer_wait_us: None,
        });
        assert_eq!(wire.len(), HEADER_SIZE);
        let wire = roundtrip(Message::Ack {
            generation: 3,
            sequence: 99,
            viewer_wait_us: Some(u32::MAX),
        });
        assert_eq!(wire.len(), HEADER_SIZE + 4);
        assert_eq!(&wire[20..], &[0xff; 4]);
        // A 2-byte ACK payload is malformed.
        let mut raw = BytesMut::new();
        raw.extend_from_slice(
            &Header {
                msg_type: MessageType::Ack,
                flags: 0,
                generation: 1,
                sequence: 1,
                payload_len: 2,
            }
            .encode(),
        );
        raw.extend_from_slice(&[0; 2]);
        assert_eq!(
            Message::decode(&raw.freeze()),
            Err(Nsl1Error::InvalidPayload("ack"))
        );
    }

    #[test]
    fn input_records_roundtrip_every_kind() {
        let events = [
            InputEvent::KeyDown { usage: 4 },
            InputEvent::KeyUp { usage: 231 },
            InputEvent::PointerMove { x: 0, y: 65535 },
            InputEvent::ButtonDown {
                button: 1,
                x: 10,
                y: 20,
            },
            InputEvent::ButtonUp {
                button: 3,
                x: 30,
                y: 40,
            },
            InputEvent::Wheel {
                axis: WheelAxis::Vertical,
                delta: -1200,
                x: 1,
                y: 2,
            },
            InputEvent::Wheel {
                axis: WheelAxis::Horizontal,
                delta: 120,
                x: 3,
                y: 4,
            },
            InputEvent::ReleaseAll,
            InputEvent::Text { scalar: 'a' },
            InputEvent::Text { scalar: '漢' },
            InputEvent::Text { scalar: '🦀' },
            InputEvent::Text { scalar: char::MAX },
            InputEvent::SmoothWheel {
                axis: WheelAxis::Vertical,
                delta: MAX_SMOOTH_WHEEL,
                x: 5,
                y: 6,
            },
            InputEvent::SmoothWheel {
                axis: WheelAxis::Horizontal,
                delta: -MAX_SMOOTH_WHEEL,
                x: 7,
                y: 8,
            },
            InputEvent::SmoothWheel {
                axis: WheelAxis::Vertical,
                delta: 1,
                x: 0,
                y: 0,
            },
            InputEvent::SmoothWheel {
                axis: WheelAxis::Horizontal,
                delta: -1,
                x: 65535,
                y: 65535,
            },
        ];
        for event in events {
            let rec = InputRecord::new(event);
            let enc = rec.encode().unwrap();
            assert_eq!(InputRecord::decode(&enc).unwrap(), rec, "{event:?}");
            roundtrip(Message::Input {
                generation: 1,
                sequence: 0,
                record: rec,
            });
        }
    }

    #[test]
    fn input_record_layout() {
        let rec = InputRecord::new(InputEvent::Wheel {
            axis: WheelAxis::Horizontal,
            delta: -120,
            x: 0x1234,
            y: 0x5678,
        })
        .with_masks(0x05, 0x81);
        assert_eq!(
            rec.encode().unwrap(),
            [6, 0, 0, 1, 0xff, 0xff, 0xff, 0x88, 0x12, 0x34, 0x56, 0x78, 0, 5, 0, 0x81]
        );
        let smooth = InputRecord::new(InputEvent::SmoothWheel {
            axis: WheelAxis::Vertical,
            delta: -40,
            x: 0x1234,
            y: 0x5678,
        })
        .with_masks(0x01, 0x02);
        assert_eq!(
            smooth.encode().unwrap(),
            [9, 0, 0, 0, 0xff, 0xff, 0xff, 0xd8, 0x12, 0x34, 0x56, 0x78, 0, 1, 0, 2]
        );
        let text = InputRecord::new(InputEvent::Text { scalar: '🦀' });
        assert_eq!(
            text.encode().unwrap(),
            [8, 0, 0, 0, 0, 0x01, 0xf9, 0x80, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn input_record_validation() {
        let bad = [
            InputEvent::KeyDown { usage: 232 },
            InputEvent::ButtonDown {
                button: 0,
                x: 0,
                y: 0,
            },
            InputEvent::ButtonUp {
                button: 4,
                x: 0,
                y: 0,
            },
            InputEvent::Wheel {
                axis: WheelAxis::Vertical,
                delta: 100,
                x: 0,
                y: 0,
            },
            InputEvent::Wheel {
                axis: WheelAxis::Vertical,
                delta: 1320,
                x: 0,
                y: 0,
            },
            InputEvent::SmoothWheel {
                axis: WheelAxis::Vertical,
                delta: 0,
                x: 0,
                y: 0,
            },
            InputEvent::SmoothWheel {
                axis: WheelAxis::Vertical,
                delta: MAX_SMOOTH_WHEEL + 1,
                x: 0,
                y: 0,
            },
            InputEvent::SmoothWheel {
                axis: WheelAxis::Horizontal,
                delta: -MAX_SMOOTH_WHEEL - 1,
                x: 0,
                y: 0,
            },
        ];
        for event in bad {
            assert!(InputRecord::new(event).encode().is_err(), "{event:?}");
        }
        assert!(InputRecord::new(InputEvent::KeyDown { usage: 1 })
            .with_masks(0x20, 0)
            .encode()
            .is_err());
        assert!(InputRecord::new(InputEvent::KeyDown { usage: 1 })
            .with_masks(0, 0x100)
            .encode()
            .is_err());
        assert!(InputRecord::new(InputEvent::ReleaseAll)
            .with_masks(1, 0)
            .encode()
            .is_err());
        assert!(InputRecord::new(InputEvent::Text { scalar: 'x' })
            .with_masks(0, 1)
            .encode()
            .is_err());
        assert!(InputRecord::new(InputEvent::SmoothWheel {
            axis: WheelAxis::Vertical,
            delta: 16,
            x: 0,
            y: 0,
        })
        .with_masks(0x20, 0)
        .encode()
        .is_err());
    }

    #[test]
    fn smooth_wheel_decode_rejects_malformed() {
        let valid = InputRecord::new(InputEvent::SmoothWheel {
            axis: WheelAxis::Vertical,
            delta: 16,
            x: 0,
            y: 0,
        })
        .encode()
        .unwrap();
        let reject = |edit: &dyn Fn(&mut [u8; INPUT_SIZE])| {
            let mut r = valid;
            edit(&mut r);
            assert!(InputRecord::decode(&r).is_err(), "{r:?}");
        };
        reject(&|r| r[4..8].copy_from_slice(&0i32.to_be_bytes()));
        reject(&|r| r[4..8].copy_from_slice(&(MAX_SMOOTH_WHEEL + 1).to_be_bytes()));
        reject(&|r| r[4..8].copy_from_slice(&(-MAX_SMOOTH_WHEEL - 1).to_be_bytes()));
        reject(&|r| r[4..8].copy_from_slice(&i32::MIN.to_be_bytes()));
        reject(&|r| r[1] = 1);
        reject(&|r| r[2..4].copy_from_slice(&2u16.to_be_bytes()));
        reject(&|r| r[13] = 0x20);
        reject(&|r| r[14] = 1);
        let mut r = valid;
        r[4..8].copy_from_slice(&(-MAX_SMOOTH_WHEEL).to_be_bytes());
        assert_eq!(
            InputRecord::decode(&r).unwrap().event,
            InputEvent::SmoothWheel {
                axis: WheelAxis::Vertical,
                delta: -MAX_SMOOTH_WHEEL,
                x: 0,
                y: 0,
            }
        );
    }

    #[test]
    fn smooth_wheel_query_is_an_empty_zero_sequence_ack() {
        let wire = roundtrip(Message::Ack {
            generation: 3,
            sequence: 0,
            viewer_wait_us: None,
        });
        assert_eq!(wire.len(), HEADER_SIZE);
        let reply = roundtrip(Message::InputResponse {
            generation: 3,
            sequence: 0,
            status: InputStatus::Accepted,
        });
        assert_eq!(&reply[12..], &[0, 0, 0, 0, 0, 0, 0, 1, 0]);
    }

    #[test]
    fn input_decode_rejects_malformed() {
        assert!(InputRecord::decode(&[0; 15]).is_err());
        assert!(InputRecord::decode(&[0; 17]).is_err());
        assert!(InputRecord::decode(&[0; 16]).is_err()); // type 0
        let mut r = InputRecord::new(InputEvent::KeyDown { usage: 1 })
            .encode()
            .unwrap();
        r[1] = 1; // reserved
        assert!(InputRecord::decode(&r).is_err());
        let mut r = InputRecord::new(InputEvent::KeyDown { usage: 1 })
            .encode()
            .unwrap();
        r[8] = 1; // stray coordinate
        assert!(InputRecord::decode(&r).is_err());
        let mut r = InputRecord::new(InputEvent::Text { scalar: 'x' })
            .encode()
            .unwrap();
        r[4..8].copy_from_slice(&0xd800u32.to_be_bytes()); // surrogate
        assert!(InputRecord::decode(&r).is_err());
        r[4..8].copy_from_slice(&0u32.to_be_bytes()); // NUL
        assert!(InputRecord::decode(&r).is_err());
        r[4..8].copy_from_slice(&0x11_0000u32.to_be_bytes()); // above range
        assert!(InputRecord::decode(&r).is_err());
        r[4..8].copy_from_slice(&(-1i32).to_be_bytes());
        assert!(InputRecord::decode(&r).is_err());
        let mut r = InputRecord::new(InputEvent::Wheel {
            axis: WheelAxis::Vertical,
            delta: 120,
            x: 0,
            y: 0,
        })
        .encode()
        .unwrap();
        r[2..4].copy_from_slice(&2u16.to_be_bytes()); // bad axis
        assert!(InputRecord::decode(&r).is_err());
        let mut r = InputRecord::new(InputEvent::ReleaseAll).encode().unwrap();
        r[15] = 1; // modifiers on release-all
        assert!(InputRecord::decode(&r).is_err());
        let mut r = InputRecord::new(InputEvent::PointerMove { x: 1, y: 1 })
            .encode()
            .unwrap();
        r[12] = 0xff; // mask out of range
        assert!(InputRecord::decode(&r).is_err());
    }

    #[test]
    fn control_ready_and_response() {
        let wire = roundtrip(Message::ControlReady {
            generation: 2,
            width: 3840,
            height: 2160,
        });
        assert_eq!(&wire[20..], &[0x0f, 0x00, 0x08, 0x70]);
        for status in [
            InputStatus::Accepted,
            InputStatus::Invalid,
            InputStatus::Unsupported,
        ] {
            roundtrip(Message::InputResponse {
                generation: 2,
                sequence: 41,
                status,
            });
        }
        let mut bad = roundtrip(Message::InputResponse {
            generation: 2,
            sequence: 1,
            status: InputStatus::Accepted,
        })
        .to_vec();
        bad[20] = 3;
        assert_eq!(
            Message::decode(&Bytes::from(bad)),
            Err(Nsl1Error::InvalidPayload("input-response"))
        );
    }

    #[test]
    fn cursor_image_and_reset() {
        let rgba = Bytes::from(vec![0xAB; 2 * 3 * 4]);
        let wire = roundtrip(Message::Cursor {
            generation: 1,
            cursor: Cursor::Image {
                width: 2,
                height: 3,
                hot_x: 1,
                hot_y: 2,
                rgba: rgba.clone(),
            },
        });
        assert_eq!(wire.len(), HEADER_SIZE + 8 + 24);
        let wire = roundtrip(Message::Cursor {
            generation: 1,
            cursor: Cursor::Reset,
        });
        assert_eq!(&wire[20..], &[0; 8]);
        // Maximum size is accepted.
        roundtrip(Message::Cursor {
            generation: 1,
            cursor: Cursor::Image {
                width: 256,
                height: 256,
                hot_x: 255,
                hot_y: 255,
                rgba: Bytes::from(vec![0; 256 * 256 * 4]),
            },
        });
    }

    #[test]
    fn cursor_rejects_invalid() {
        let image = |w, h, hx, hy, len| Message::Cursor {
            generation: 1,
            cursor: Cursor::Image {
                width: w,
                height: h,
                hot_x: hx,
                hot_y: hy,
                rgba: Bytes::from(vec![0; len]),
            },
        };
        assert!(image(257, 1, 0, 0, 257 * 4).encode().is_err()); // too wide
        assert!(image(1, 257, 0, 0, 257 * 4).encode().is_err()); // too tall
        assert!(image(2, 2, 2, 0, 16).encode().is_err()); // hotspot outside
        assert!(image(2, 2, 0, 2, 16).encode().is_err());
        assert!(image(2, 2, 0, 0, 15).encode().is_err()); // wrong pixel length
        assert!(image(0, 0, 0, 0, 0).encode().is_err()); // use Cursor::Reset
                                                         // Decoding a nonzero prefix with zero size is invalid, as is a truncated prefix.
        let mut raw = BytesMut::new();
        raw.extend_from_slice(
            &Header {
                msg_type: MessageType::Cursor,
                flags: 0,
                generation: 1,
                sequence: 0,
                payload_len: 8,
            }
            .encode(),
        );
        raw.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0, 0]);
        assert!(Message::decode(&raw.freeze()).is_err());
        let mut raw = BytesMut::new();
        raw.extend_from_slice(
            &Header {
                msg_type: MessageType::Cursor,
                flags: 0,
                generation: 1,
                sequence: 0,
                payload_len: 4,
            }
            .encode(),
        );
        raw.extend_from_slice(&[0; 4]);
        assert!(Message::decode(&raw.freeze()).is_err());
    }

    #[test]
    fn clipboard_notice() {
        let wire = roundtrip(Message::ClipboardNotice { generation: 5 });
        assert_eq!(wire.len(), HEADER_SIZE);
        let mut bad = wire.to_vec();
        bad[15] = 1; // nonzero sequence
        assert!(Message::decode(&Bytes::from(bad)).is_err());
        let mut bad = wire.to_vec();
        bad[19] = 1;
        bad.push(0); // payload on a notice
        assert!(Message::decode(&Bytes::from(bad)).is_err());
    }

    fn sl1_wire(msg_type: MessageType, payload: &[u8]) -> Bytes {
        let mut wire = BytesMut::new();
        wire.extend_from_slice(
            &Header {
                msg_type,
                flags: 0,
                generation: 23,
                sequence: 0,
                payload_len: payload.len() as u32,
            }
            .encode(),
        );
        wire.extend_from_slice(payload);
        wire.freeze()
    }

    fn monitor() -> MonitorInfo {
        MonitorInfo {
            id: "output-雪".into(),
            name: "Écran".into(),
            x: -1920,
            y: i32::MIN,
            width: 1920,
            height: 1080,
            primary: true,
            active: false,
        }
    }

    #[test]
    fn sl1_roundtrips_and_wire_layouts() {
        let list = roundtrip(Message::MonitorList {
            generation: 23,
            monitors: vec![monitor()],
        });
        let expected = r#"[{"id":"output-雪","name":"Écran","x":-1920,"y":-2147483648,"width":1920,"height":1080,"primary":true,"active":false}]"#;
        assert_eq!(
            list,
            sl1_wire(MessageType::MonitorList, expected.as_bytes())
        );
        assert_eq!(&list[..8], b"NSL1\x01\x11\0\0");
        assert_eq!(&list[8..16], &[0, 0, 0, 23, 0, 0, 0, 0]);
        let list = roundtrip(Message::MonitorList {
            generation: 23,
            monitors: vec![],
        });
        assert_eq!(&list[HEADER_SIZE..], b"[]");

        for (message, type_byte, payload) in [
            (
                Message::SelectMonitor {
                    generation: 23,
                    id: "output-雪".into(),
                },
                18,
                "output-雪",
            ),
            (
                Message::ClipboardData {
                    generation: 23,
                    text: "café\0雪\n".into(),
                },
                19,
                "café\0雪\n",
            ),
            (
                Message::ClipboardData {
                    generation: 23,
                    text: String::new(),
                },
                19,
                "",
            ),
        ] {
            let wire = roundtrip(message);
            assert_eq!(&wire[4..8], &[1, type_byte, 0, 0]);
            assert_eq!(&wire[8..16], &[0, 0, 0, 23, 0, 0, 0, 0]);
            assert_eq!(&wire[HEADER_SIZE..], payload.as_bytes());
        }
    }

    #[test]
    fn sl1_enforces_byte_limits_on_encode_and_decode() {
        let list = |monitor| Message::MonitorList {
            generation: 23,
            monitors: vec![monitor],
        };
        let mut item = monitor();
        item.name.clear();
        let overhead = list(item.clone()).encode().unwrap().len() - HEADER_SIZE;
        item.name = "a".repeat(MAX_MONITOR_LIST - overhead);
        assert_eq!(
            roundtrip(list(item.clone())).len(),
            HEADER_SIZE + MAX_MONITOR_LIST
        );
        item.name.push('a');
        let json = serde_json::to_vec(&[&item]).unwrap();
        assert_eq!(
            list(item).encode(),
            Err(Nsl1Error::PayloadTooLarge(MAX_MONITOR_LIST + 1))
        );

        let select = |id| Message::SelectMonitor { generation: 23, id };
        let clip = |text| Message::ClipboardData {
            generation: 23,
            text,
        };
        roundtrip(select("é".repeat(MAX_MONITOR_ID / 2)));
        roundtrip(clip("é".repeat(MAX_CLIPBOARD_DATA / 2)));
        assert_eq!(
            select(String::new()).encode(),
            Err(Nsl1Error::InvalidPayload("select monitor"))
        );
        assert_eq!(
            select("a".repeat(MAX_MONITOR_ID + 1)).encode(),
            Err(Nsl1Error::PayloadTooLarge(MAX_MONITOR_ID + 1))
        );
        assert_eq!(
            clip("a".repeat(MAX_CLIPBOARD_DATA + 1)).encode(),
            Err(Nsl1Error::PayloadTooLarge(MAX_CLIPBOARD_DATA + 1))
        );

        for (kind, payload) in [
            (MessageType::MonitorList, json),
            (MessageType::SelectMonitor, vec![b'a'; MAX_MONITOR_ID + 1]),
            (
                MessageType::ClipboardData,
                vec![b'a'; MAX_CLIPBOARD_DATA + 1],
            ),
        ] {
            let wire = sl1_wire(kind, &payload);
            let error = Nsl1Error::PayloadTooLarge(payload.len());
            assert_eq!(Header::decode(&wire[..HEADER_SIZE]), Err(error.clone()));
            assert_eq!(Message::decode(&wire), Err(error));
        }
    }

    #[test]
    fn sl1_rejects_nonzero_sequence_or_flags() {
        for (kind, payload) in [
            (MessageType::MonitorList, &b"[]"[..]),
            (MessageType::SelectMonitor, &b"display"[..]),
            (MessageType::ClipboardData, &b"text"[..]),
        ] {
            let wire = sl1_wire(kind, payload);
            let mut flagged = wire.to_vec();
            flagged[7] = 1;
            assert_eq!(
                Message::decode(&flagged.into()),
                Err(Nsl1Error::BadFlags(1))
            );
            let mut sequenced = wire.to_vec();
            sequenced[15] = 1;
            assert_eq!(
                Message::decode(&sequenced.into()),
                Err(Nsl1Error::NonzeroSequence)
            );
        }
    }

    #[test]
    fn sl1_rejects_invalid_utf8_and_monitor_json() {
        let list = MessageType::MonitorList;
        for payload in [
            &b""[..],
            &b"["[..],
            &b"{}"[..],
            &b"[{}]"[..],
            &b"[[]]"[..],
            &b"[]true"[..],
            &b"[\xFF]"[..],
            &br#"[{"id":"a","name":"a","x":0,"y":0,"width":65536,"height":1,"primary":true,"active":true}]"#[..],
        ] {
            assert_eq!(Message::decode(&sl1_wire(list, payload)), Err(Nsl1Error::InvalidPayload("monitor list")));
        }
        for (kind, name) in [
            (MessageType::SelectMonitor, "select monitor"),
            (MessageType::ClipboardData, "clipboard data"),
        ] {
            assert_eq!(
                Message::decode(&sl1_wire(kind, b"\xc3(")),
                Err(Nsl1Error::InvalidPayload(name))
            );
        }
        assert_eq!(
            Message::decode(&sl1_wire(MessageType::SelectMonitor, b"")),
            Err(Nsl1Error::InvalidPayload("select monitor"))
        );
    }

    fn audio_frame(sequence: u32, data: &'static [u8]) -> Message {
        Message::AudioFrame {
            generation: 23,
            sequence,
            capture_us: 0x0102_0304_0506_0708,
            data: Bytes::from_static(data),
        }
    }

    #[test]
    fn audio_roundtrips_and_wire_layouts() {
        let config = roundtrip(Message::AudioConfig {
            generation: 23,
            format: AudioFormat::opus(),
        });
        assert_eq!(
            config,
            sl1_wire(
                MessageType::AudioConfig,
                br#"{"codec":"opus","sample_rate":48000,"channels":2,"frame_ms":20}"#
            )
        );
        let frame = roundtrip(audio_frame(7, &[0xfc, 0xff, 0xfe]));
        assert_eq!(&frame[4..8], &[1, 21, 0, 0]);
        assert_eq!(&frame[8..20], &[0, 0, 0, 23, 0, 0, 0, 7, 0, 0, 0, 11]);
        assert_eq!(&frame[20..], &[1, 2, 3, 4, 5, 6, 7, 8, 0xfc, 0xff, 0xfe]);
        let Message::AudioFrame { data, .. } = Message::decode(&frame).unwrap() else {
            panic!("not an audio frame");
        };
        assert_eq!(
            data.as_ptr(),
            frame[HEADER_SIZE + AUDIO_FRAME_PREFIX_SIZE..].as_ptr()
        );
        for (enabled, byte) in [(true, 1), (false, 0)] {
            let wire = roundtrip(Message::AudioControl {
                generation: 23,
                enabled,
            });
            assert_eq!(wire, sl1_wire(MessageType::AudioControl, &[byte]));
        }
    }

    #[test]
    fn audio_config_accepts_extra_members_and_rejects_bad_json() {
        let extended =
            br#"{"frame_ms":10,"channels":1,"sample_rate":24000,"codec":"opus","dtx":true}"#;
        assert_eq!(
            Message::decode(&sl1_wire(MessageType::AudioConfig, extended)),
            Ok(Message::AudioConfig {
                generation: 23,
                format: AudioFormat {
                    codec: "opus".into(),
                    sample_rate: 24_000,
                    channels: 1,
                    frame_ms: 10,
                },
            })
        );
        for payload in [
            &b""[..],
            &b"[]"[..],
            &br#"["opus",48000,2,20]"#[..],
            &br#"{"codec":"opus","sample_rate":48000,"channels":2}"#[..],
            &br#"{"codec":"opus","sample_rate":48000,"channels":256,"frame_ms":20}"#[..],
            &b"{\"codec\":\"\xff\"}"[..],
        ] {
            assert_eq!(
                Message::decode(&sl1_wire(MessageType::AudioConfig, payload)),
                Err(Nsl1Error::InvalidPayload("audio config"))
            );
        }
        let mut format = AudioFormat::opus();
        format.codec = "x".repeat(MAX_AUDIO_CONFIG);
        assert!(matches!(
            Message::AudioConfig {
                generation: 23,
                format,
            }
            .encode(),
            Err(Nsl1Error::PayloadTooLarge(_))
        ));
        let oversized = vec![b' '; MAX_AUDIO_CONFIG + 1];
        assert_eq!(
            Message::decode(&sl1_wire(MessageType::AudioConfig, &oversized)),
            Err(Nsl1Error::PayloadTooLarge(MAX_AUDIO_CONFIG + 1))
        );
    }

    #[test]
    fn audio_frame_limits() {
        static LARGEST: [u8; MAX_AUDIO_FRAME - AUDIO_FRAME_PREFIX_SIZE] =
            [0; MAX_AUDIO_FRAME - AUDIO_FRAME_PREFIX_SIZE];
        static TOO_LARGE: [u8; MAX_AUDIO_FRAME - AUDIO_FRAME_PREFIX_SIZE + 1] =
            [0; MAX_AUDIO_FRAME - AUDIO_FRAME_PREFIX_SIZE + 1];
        assert_eq!(
            roundtrip(audio_frame(u32::MAX, &LARGEST)).len(),
            HEADER_SIZE + MAX_AUDIO_FRAME
        );
        assert_eq!(
            audio_frame(1, &TOO_LARGE).encode(),
            Err(Nsl1Error::PayloadTooLarge(MAX_AUDIO_FRAME + 1))
        );
        assert_eq!(
            audio_frame(1, &[]).encode(),
            Err(Nsl1Error::InvalidPayload("audio frame"))
        );
        assert_eq!(
            audio_frame(0, &[1]).encode(),
            Err(Nsl1Error::InvalidPayload("audio frame"))
        );
        let wire = |sequence: u32, payload: &[u8]| {
            let mut raw = BytesMut::new();
            raw.extend_from_slice(
                &Header {
                    msg_type: MessageType::AudioFrame,
                    flags: 0,
                    generation: 1,
                    sequence,
                    payload_len: payload.len() as u32,
                }
                .encode(),
            );
            raw.extend_from_slice(payload);
            raw.freeze()
        };
        for (sequence, payload) in [(1, &[0u8; 8][..]), (1, &[0u8; 3][..]), (0, &[0u8; 9][..])] {
            assert_eq!(
                Message::decode(&wire(sequence, payload)),
                Err(Nsl1Error::InvalidPayload("audio frame"))
            );
        }
        let oversized = wire(1, &[0; MAX_AUDIO_FRAME + 1]);
        assert_eq!(
            Header::decode(&oversized),
            Err(Nsl1Error::PayloadTooLarge(MAX_AUDIO_FRAME + 1))
        );
        let mut flagged = wire(1, &[0; 9]).to_vec();
        flagged[7] = 1;
        assert_eq!(
            Message::decode(&flagged.into()),
            Err(Nsl1Error::BadFlags(1))
        );
    }

    #[test]
    fn audio_control_and_config_reject_malformed_headers() {
        for payload in [&[2u8][..], &[][..]] {
            assert_eq!(
                Message::decode(&sl1_wire(MessageType::AudioControl, payload)),
                Err(Nsl1Error::InvalidPayload("audio control"))
            );
        }
        assert_eq!(
            Message::decode(&sl1_wire(MessageType::AudioControl, &[1, 0])),
            Err(Nsl1Error::PayloadTooLarge(2))
        );
        for (kind, payload) in [
            (MessageType::AudioControl, &[1u8][..]),
            (
                MessageType::AudioConfig,
                &br#"{"codec":"opus","sample_rate":48000,"channels":2,"frame_ms":20}"#[..],
            ),
        ] {
            let mut sequenced = sl1_wire(kind, payload).to_vec();
            sequenced[15] = 1;
            assert_eq!(
                Message::decode(&sequenced.into()),
                Err(Nsl1Error::NonzeroSequence)
            );
            let mut flagged = sl1_wire(kind, payload).to_vec();
            flagged[7] = 1;
            assert_eq!(
                Message::decode(&flagged.into()),
                Err(Nsl1Error::BadFlags(1))
            );
        }
    }

    #[test]
    fn decode_rejects_length_mismatch_and_zero_generation_encode() {
        let wire = Message::ClipboardNotice { generation: 1 }.encode().unwrap();
        let mut long = wire.to_vec();
        long.push(0);
        assert_eq!(
            Message::decode(&Bytes::from(long)),
            Err(Nsl1Error::LengthMismatch {
                declared: 0,
                actual: 1
            })
        );
        assert!(Message::decode(&Bytes::from_static(b"NSL1")).is_err());
        assert_eq!(
            Message::ClipboardNotice { generation: 0 }.encode(),
            Err(Nsl1Error::ZeroGeneration)
        );
    }
}
