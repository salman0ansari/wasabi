//! 16 kHz mono Opus in an Ogg container (WhatsApp PTT shape).

use std::io::{Cursor, Read};

pub const SAMPLE_RATE: u32 = 16_000;
pub const FRAME_SAMPLES: usize = 320; // 20 ms at 16 kHz
pub const VOICE_MIME: &str = "audio/ogg; codecs=opus";
const OPUS_48KHZ: u32 = 48_000;
const STREAM_SERIAL: u32 = 0x5741_5342; // "WASB"
const MAX_OPUS_PACKET: usize = 4000;

#[derive(Debug)]
pub enum EncodeError {
    Codec(&'static str),
    Container(&'static str),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Codec(detail) | Self::Container(detail) => f.write_str(detail),
        }
    }
}

impl std::error::Error for EncodeError {}

pub struct DecodedPcm {
    pub samples: Vec<i16>,
    pub sample_rate: u32,
}

pub struct EncodedVoice {
    pub bytes: Vec<u8>,
    pub duration_seconds: u32,
}

pub fn encode_pcm_to_ogg_opus(samples: &[i16], sample_rate: u32) -> Result<Vec<u8>, EncodeError> {
    if samples.is_empty() {
        return Err(EncodeError::Codec("recording is empty"));
    }
    let pcm = if sample_rate == SAMPLE_RATE {
        samples.to_vec()
    } else {
        let as_f32 = samples
            .iter()
            .map(|sample| *sample as f32 / 32768.0)
            .collect::<Vec<_>>();
        f32_to_i16(&resample_linear(&as_f32, sample_rate, SAMPLE_RATE))
    };
    encode_16k_mono(&pcm)
}

pub fn decode_ogg_opus(data: &[u8]) -> Result<DecodedPcm, EncodeError> {
    if !is_ogg_opus(data) {
        return Err(EncodeError::Container("not an Ogg Opus stream"));
    }
    let packets = read_ogg_packets(data)?;
    if packets.is_empty() {
        return Err(EncodeError::Container("empty Ogg stream"));
    }
    let (channels, sample_rate) = parse_opus_head(&packets[0])?;
    if channels != 1 {
        return Err(EncodeError::Codec("only mono Opus is supported"));
    }
    let mut decoder = opus::Decoder::new(sample_rate, opus::Channels::Mono)
        .map_err(|_| EncodeError::Codec("could not start Opus decoder"))?;
    let mut pcm = Vec::new();
    let mut frame = vec![0i16; (sample_rate as usize / 25).max(5760)];
    for packet in packets.iter().skip(1) {
        if packet.starts_with(b"OpusTags") || packet.starts_with(b"OpusHead") {
            continue;
        }
        let n = decoder
            .decode(packet, &mut frame, false)
            .map_err(|_| EncodeError::Codec("could not decode Opus packet"))?;
        pcm.extend_from_slice(&frame[..n]);
    }
    if pcm.is_empty() {
        return Err(EncodeError::Codec("decoded recording is empty"));
    }
    Ok(DecodedPcm { samples: pcm, sample_rate })
}

pub fn is_ogg_opus(data: &[u8]) -> bool {
    data.len() > 36 && data.starts_with(b"OggS") && data.windows(8).any(|w| w == b"OpusHead")
}

pub fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == 0 {
        return Vec::new();
    }
    if from == to {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let out_len = ((input.len() as f64) / ratio).round().max(1.0) as usize;
    let last = input.len() - 1;
    (0..out_len)
        .map(|i| {
            let src = i as f64 * ratio;
            let i0 = (src.floor() as usize).min(last);
            let i1 = (i0 + 1).min(last);
            let frac = (src - i0 as f64) as f32;
            input[i0] * (1.0 - frac) + input[i1] * frac
        })
        .collect()
}

pub fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|sample| {
            let clamped = sample.clamp(-1.0, 1.0);
            (clamped * 32767.0).round() as i16
        })
        .collect()
}

pub fn duration_seconds(sample_count: usize, sample_rate: u32) -> u32 {
    if sample_rate == 0 || sample_count == 0 {
        return 0;
    }
    (sample_count as u64).div_ceil(sample_rate as u64) as u32
}

fn encode_16k_mono(samples: &[i16]) -> Result<Vec<u8>, EncodeError> {
    let mut encoder = opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip)
        .map_err(|_| EncodeError::Codec("could not start Opus encoder"))?;
    let _ = encoder.set_bitrate(opus::Bitrate::Bits(16_000));
    let lookahead = encoder.get_lookahead().unwrap_or(104).max(0) as u32;
    let pre_skip = lookahead.saturating_mul(OPUS_48KHZ / SAMPLE_RATE) as u16;
    let mut writer = OggWriter::new(STREAM_SERIAL);
    writer.write_packet(&opus_head(pre_skip), 0, true, false)?;
    writer.write_packet(&opus_tags(), 0, false, false)?;

    let mut packet = [0u8; MAX_OPUS_PACKET];
    let mut granule: u64 = 0;
    let granule_step = (FRAME_SAMPLES as u64) * (OPUS_48KHZ / SAMPLE_RATE) as u64;
    let mut offset = 0;
    while offset < samples.len() {
        let end = (offset + FRAME_SAMPLES).min(samples.len());
        let mut frame = [0i16; FRAME_SAMPLES];
        frame[..end - offset].copy_from_slice(&samples[offset..end]);
        let n = encoder
            .encode(&frame, &mut packet)
            .map_err(|_| EncodeError::Codec("could not encode Opus frame"))?;
        granule += granule_step;
        let last = end == samples.len();
        writer.write_packet(&packet[..n], granule, false, last)?;
        offset = end;
    }
    Ok(writer.into_bytes())
}

fn opus_head(pre_skip: u16) -> Vec<u8> {
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1);
    head.push(1);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes());
    head.push(0);
    head
}

fn opus_tags() -> Vec<u8> {
    let vendor = b"wasabi";
    let mut tags = Vec::with_capacity(16 + vendor.len());
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes());
    tags
}

fn parse_opus_head(packet: &[u8]) -> Result<(u8, u32), EncodeError> {
    if packet.len() < 19 || !packet.starts_with(b"OpusHead") {
        return Err(EncodeError::Container("missing Opus identification header"));
    }
    let channels = packet[9];
    let sample_rate = u32::from_le_bytes(packet[12..16].try_into().unwrap_or([0; 4]));
    let rate = match sample_rate {
        8_000 | 12_000 | 16_000 | 24_000 | 48_000 => sample_rate,
        _ => SAMPLE_RATE,
    };
    Ok((channels, rate))
}

struct OggWriter {
    serial: u32,
    sequence: u32,
    out: Vec<u8>,
}

impl OggWriter {
    fn new(serial: u32) -> Self {
        Self {
            serial,
            sequence: 0,
            out: Vec::new(),
        }
    }

    fn write_packet(
        &mut self,
        packet: &[u8],
        granule: u64,
        bos: bool,
        eos: bool,
    ) -> Result<(), EncodeError> {
        let mut header_type = 0u8;
        if bos {
            header_type |= 0x02;
        }
        if eos {
            header_type |= 0x04;
        }
        let mut segments = Vec::new();
        let mut remaining = packet.len();
        while remaining >= 255 {
            segments.push(255);
            remaining -= 255;
        }
        segments.push(remaining as u8);
        if segments.len() > 255 {
            return Err(EncodeError::Container("Opus packet is too large"));
        }
        let mut page = Vec::with_capacity(27 + segments.len() + packet.len());
        page.extend_from_slice(b"OggS");
        page.push(0);
        page.push(header_type);
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&self.serial.to_le_bytes());
        page.extend_from_slice(&self.sequence.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes());
        page.push(segments.len() as u8);
        page.extend_from_slice(&segments);
        page.extend_from_slice(packet);
        let crc = ogg_crc(&page);
        page[22..26].copy_from_slice(&crc.to_le_bytes());
        self.out.extend_from_slice(&page);
        self.sequence = self.sequence.wrapping_add(1);
        Ok(())
    }

    fn into_bytes(self) -> Vec<u8> {
        self.out
    }
}

fn read_ogg_packets(data: &[u8]) -> Result<Vec<Vec<u8>>, EncodeError> {
    let mut cursor = Cursor::new(data);
    let mut packets = Vec::new();
    let mut current = Vec::new();
    loop {
        let mut capture = [0u8; 4];
        match cursor.read_exact(&mut capture) {
            Ok(()) => {}
            Err(_) => break,
        }
        if &capture != b"OggS" {
            return Err(EncodeError::Container("corrupt Ogg page"));
        }
        let mut header = [0u8; 23];
        cursor
            .read_exact(&mut header)
            .map_err(|_| EncodeError::Container("truncated Ogg page"))?;
        let nseg = header[22] as usize;
        let mut segments = vec![0u8; nseg];
        cursor
            .read_exact(&mut segments)
            .map_err(|_| EncodeError::Container("truncated Ogg segment table"))?;
        for len in segments {
            let len = len as usize;
            let mut chunk = vec![0u8; len];
            cursor
                .read_exact(&mut chunk)
                .map_err(|_| EncodeError::Container("truncated Ogg packet"))?;
            current.extend_from_slice(&chunk);
            if len < 255 {
                packets.push(std::mem::take(&mut current));
            }
        }
    }
    if !current.is_empty() {
        packets.push(current);
    }
    Ok(packets)
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut r = (i as u32) << 24;
        let mut j = 0;
        while j < 8 {
            if r & 0x8000_0000 != 0 {
                r = (r << 1) ^ 0x04c1_1db7;
            } else {
                r <<= 1;
            }
            j += 1;
        }
        table[i] = r;
        i += 1;
    }
    table
}

const CRC_TABLE: [u32; 256] = crc_table();

fn ogg_crc(page: &[u8]) -> u32 {
    let mut crc = 0u32;
    for (i, byte) in page.iter().enumerate() {
        let value = if (22..26).contains(&i) { 0 } else { *byte };
        crc = (crc << 8) ^ CRC_TABLE[(((crc >> 24) as u8) ^ value) as usize];
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(seconds: f32) -> Vec<i16> {
        let n = (seconds * SAMPLE_RATE as f32).round() as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                (t * 440.0 * 2.0 * std::f32::consts::PI).sin() * 0.4 * 32767.0
            })
            .map(|sample| sample.round() as i16)
            .collect()
    }

    fn rms(samples: &[i16]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum = samples
            .iter()
            .map(|sample| *sample as f64 * *sample as f64)
            .sum::<f64>();
        (sum / samples.len() as f64).sqrt() as f32
    }

    #[test]
    fn ogg_opus_roundtrip_keeps_audible_energy() {
        let original = sine(0.3);
        let encoded = encode_pcm_to_ogg_opus(&original, SAMPLE_RATE).expect("encode");
        assert!(encoded.starts_with(b"OggS"));
        assert!(encoded.windows(8).any(|window| window == b"OpusHead"));
        let decoded = decode_ogg_opus(&encoded).expect("decode");
        assert_eq!(decoded.sample_rate, SAMPLE_RATE);
        assert!(decoded.samples.len() >= original.len() / 2);
        assert!(rms(&decoded.samples) > 500.0);
        assert!(duration_seconds(original.len(), SAMPLE_RATE) >= 1);
    }

    #[test]
    fn resample_is_deterministic_and_preserves_length_ratio() {
        let input = vec![0.0, 1.0, 0.0, -1.0];
        let doubled = resample_linear(&input, 8_000, 16_000);
        assert_eq!(doubled.len(), 8);
        assert!((doubled[0]).abs() < f32::EPSILON);
    }

    #[test]
    fn empty_pcm_fails_honestly() {
        assert!(encode_pcm_to_ogg_opus(&[], SAMPLE_RATE).is_err());
        assert!(decode_ogg_opus(b"not ogg").is_err());
    }
}
