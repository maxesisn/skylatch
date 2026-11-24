//! Opus decoding of the host's audio stream into interleaved 16-bit PCM.

use opusic_sys as opus;
use skylatch_proto::nsl1::AudioFormat;
use std::{ffi::c_int, ptr::NonNull};

/// Longest Opus packet duration.
const MAX_PACKET_MS: usize = 120;
/// Lost packets in a row that are concealed. After a longer gap playback resumes at the next
/// packet, since minutes of synthesized audio would only delay it.
const MAX_CONCEALED: u32 = 5;

/// Decoded audio: `samples` holds `channels` interleaved samples per sampling instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPcm {
    /// Host capture time in microseconds of the first sample.
    pub capture_us: u64,
    pub sample_rate: u32,
    pub channels: u8,
    pub samples: Vec<i16>,
}

impl AudioPcm {
    pub fn samples_per_channel(&self) -> usize {
        self.samples.len() / usize::from(self.channels)
    }
}

pub(crate) struct AudioDecoder {
    state: NonNull<opus::OpusDecoder>,
    sample_rate: u32,
    channels: u8,
    frame_us: u64,
    frame_samples: usize,
    last_sequence: u32,
}

// SAFETY: the decoder state is owned by this value and only used through `&mut self`.
unsafe impl Send for AudioDecoder {}

impl AudioDecoder {
    /// A decoder for `format`, or `None` when it is not an Opus format libopus can decode.
    pub(crate) fn new(format: &AudioFormat) -> Option<Self> {
        let supported = format.codec == "opus"
            && matches!(format.sample_rate, 8000 | 12000 | 16000 | 24000 | 48000)
            && matches!(format.channels, 1 | 2)
            && matches!(format.frame_ms, 10 | 20 | 40 | 60 | 80 | 100 | 120);
        if !supported {
            return None;
        }
        let mut error = 0;
        // SAFETY: the rate and channel count were validated above and `error` is writable.
        let state = unsafe {
            opus::opus_decoder_create(
                format.sample_rate as c_int,
                c_int::from(format.channels),
                &mut error,
            )
        };
        let state = NonNull::new(state)?;
        let decoder = Self {
            state,
            sample_rate: format.sample_rate,
            channels: format.channels,
            frame_us: u64::from(format.frame_ms) * 1000,
            frame_samples: format.sample_rate as usize * usize::from(format.frame_ms) / 1000,
            last_sequence: 0,
        };
        (error == opus::OPUS_OK).then_some(decoder)
    }

    /// Decodes one packet. Up to [`MAX_CONCEALED`] packets missing before it are first replaced
    /// by packet loss concealment, each returned as its own block.
    pub(crate) fn decode(
        &mut self,
        sequence: u32,
        capture_us: u64,
        packet: &[u8],
    ) -> Vec<AudioPcm> {
        let lost = match self.last_sequence {
            0 => 0,
            last => sequence.wrapping_sub(last).wrapping_sub(1),
        };
        self.last_sequence = sequence;
        let mut blocks = Vec::new();
        if (1..=MAX_CONCEALED).contains(&lost) {
            for missing in (1..=u64::from(lost)).rev() {
                let start = capture_us.saturating_sub(missing * self.frame_us);
                blocks.extend(self.run(None, self.frame_samples, start));
            }
        }
        let max = self.sample_rate as usize * MAX_PACKET_MS / 1000;
        blocks.extend(self.run(Some(packet), max, capture_us));
        blocks
    }

    fn run(
        &mut self,
        packet: Option<&[u8]>,
        frame_size: usize,
        capture_us: u64,
    ) -> Option<AudioPcm> {
        let mut samples = vec![0i16; frame_size * usize::from(self.channels)];
        let (data, len) = packet.map_or((std::ptr::null(), 0), |p| (p.as_ptr(), p.len()));
        let len = c_int::try_from(len).ok()?;
        // SAFETY: `samples` holds `frame_size` samples for every channel, and `data` is null (loss
        // concealment) or points to `len` readable bytes.
        let decoded = unsafe {
            opus::opus_decode(
                self.state.as_ptr(),
                data,
                len,
                samples.as_mut_ptr(),
                frame_size as c_int,
                0,
            )
        };
        let decoded = usize::try_from(decoded).ok().filter(|&n| n > 0)?;
        samples.truncate(decoded * usize::from(self.channels));
        Some(AudioPcm {
            capture_us,
            sample_rate: self.sample_rate,
            channels: self.channels,
            samples,
        })
    }
}

impl Drop for AudioDecoder {
    fn drop(&mut self) {
        // SAFETY: the state came from opus_decoder_create and is destroyed exactly once.
        unsafe { opus::opus_decoder_destroy(self.state.as_ptr()) };
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Encodes `packets` 20 ms packets of a 440 Hz stereo tone with libopus.
    pub(crate) fn tone_packets(packets: usize) -> Vec<Vec<u8>> {
        let mut error = 0;
        let encoder = unsafe {
            opus::opus_encoder_create(48_000, 2, opus::OPUS_APPLICATION_AUDIO, &mut error)
        };
        assert!(!encoder.is_null() && error == opus::OPUS_OK);
        let mut phase = 0f32;
        let out = (0..packets)
            .map(|_| {
                let pcm: Vec<i16> = (0..960)
                    .flat_map(|_| {
                        phase += 440.0 * std::f32::consts::TAU / 48_000.0;
                        let s = (phase.sin() * 12_000.0) as i16;
                        [s, s]
                    })
                    .collect();
                let mut packet = vec![0u8; 4000];
                let n = unsafe {
                    opus::opus_encode(encoder, pcm.as_ptr(), 960, packet.as_mut_ptr(), 4000)
                };
                packet.truncate(usize::try_from(n).unwrap());
                packet
            })
            .collect();
        unsafe { opus::opus_encoder_destroy(encoder) };
        out
    }

    fn energy(pcm: &AudioPcm) -> f64 {
        pcm.samples
            .iter()
            .map(|&s| f64::from(s).powi(2))
            .sum::<f64>()
            / pcm.samples.len() as f64
    }

    #[test]
    fn rejects_formats_libopus_cannot_decode() {
        let base = AudioFormat::opus();
        for format in [
            AudioFormat {
                codec: "aac".into(),
                ..base.clone()
            },
            AudioFormat {
                sample_rate: 44_100,
                ..base.clone()
            },
            AudioFormat {
                channels: 3,
                ..base.clone()
            },
            AudioFormat {
                frame_ms: 0,
                ..base.clone()
            },
        ] {
            assert!(AudioDecoder::new(&format).is_none(), "{format:?}");
        }
        assert!(AudioDecoder::new(&base).is_some());
    }

    #[test]
    fn decodes_a_tone_and_conceals_short_gaps() {
        let packets = tone_packets(12);
        let mut decoder = AudioDecoder::new(&AudioFormat::opus()).unwrap();
        let mut blocks = Vec::new();
        for (index, packet) in packets.iter().enumerate().take(8) {
            blocks.extend(decoder.decode(index as u32 + 1, index as u64 * 20_000, packet));
        }
        assert_eq!(blocks.len(), 8);
        assert!(blocks
            .iter()
            .all(|b| b.samples_per_channel() == 960 && b.channels == 2));
        assert!(energy(&blocks[7]) > 1e6, "{}", energy(&blocks[7]));

        // Packets 9 and 10 are lost: two concealed blocks precede packet 11.
        let concealed = decoder.decode(11, 200_000, &packets[10]);
        assert_eq!(
            concealed.iter().map(|b| b.capture_us).collect::<Vec<_>>(),
            [160_000, 180_000, 200_000]
        );
        assert!(concealed.iter().all(|b| b.samples_per_channel() == 960));

        // A long gap resumes at once.
        assert_eq!(decoder.decode(100, 2_000_000, &packets[11]).len(), 1);
    }
}
