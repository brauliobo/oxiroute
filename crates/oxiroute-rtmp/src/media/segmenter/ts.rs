#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct TsMuxer {
    bytes: Vec<u8>,
    video: AvcConfig,
    audio: AacConfig,
    continuity: HashMap<u16, u8>,
}

impl TsMuxer {
    pub(super) fn new(video: AvcConfig, audio: AacConfig) -> Self {
        let mut muxer = Self {
            bytes: Vec::with_capacity(TS_PACKET_BYTES * 16),
            video,
            audio,
            continuity: HashMap::new(),
        };
        muxer.write_pat();
        muxer.write_pmt();
        muxer
    }

    pub(super) fn len(&self) -> usize {
        self.bytes.len()
    }

    pub(super) fn write_video(
        &mut self,
        event: &MediaEvent,
        timestamp_ms: u32,
        start_timestamp_ms: u32,
        keyframe: bool,
    ) -> Result<(), ()> {
        let payload = event.payload();
        if payload.len() < 9 || payload[1] != 1 {
            return Err(());
        }
        let composition_time = signed_24(&payload[2..5]);
        let mut access_unit = Vec::new();
        if keyframe {
            access_unit.extend_from_slice(&[0, 0, 0, 1, 0x09, 0xf0]);
            for nalu in self.video.sps.iter().chain(&self.video.pps) {
                access_unit.extend_from_slice(&[0, 0, 0, 1]);
                access_unit.extend_from_slice(nalu);
            }
        }
        append_length_prefixed_nalus(&mut access_unit, &payload[5..], self.video.nal_length_size)?;
        if access_unit.is_empty() {
            return Err(());
        }
        let dts = relative_timestamp(timestamp_ms, start_timestamp_ms);
        let pts =
            relative_timestamp_with_composition(timestamp_ms, composition_time, start_timestamp_ms);
        self.write_pes(VIDEO_PID, 0xe0, pts, dts, &access_unit);
        Ok(())
    }

    pub(super) fn write_audio(
        &mut self,
        event: &MediaEvent,
        timestamp_ms: u32,
        start_timestamp_ms: u32,
    ) -> Result<(), ()> {
        let payload = event.payload();
        if payload.len() < 3 || payload[0] >> 4 != 10 || payload[1] != 1 {
            return Err(());
        }
        let raw = &payload[2..];
        let frame_length = raw.len().checked_add(7).ok_or(())?;
        if frame_length > 0x1fff {
            return Err(());
        }
        let mut adts = [0_u8; 7];
        adts[0] = 0xff;
        adts[1] = 0xf1;
        adts[2] = (self.audio.profile << 6)
            | (self.audio.sample_rate_index << 2)
            | (self.audio.channel_configuration >> 2);
        adts[3] = ((self.audio.channel_configuration & 0x03) << 6)
            | u8::try_from((frame_length >> 11) & 0x03).expect("ADTS length bits fit");
        adts[4] = u8::try_from((frame_length >> 3) & 0xff).expect("ADTS length bits fit");
        adts[5] = u8::try_from((frame_length & 0x07) << 5).expect("ADTS length bits fit") | 0x1f;
        adts[6] = 0xfc;
        let mut frame = Vec::with_capacity(frame_length);
        frame.extend_from_slice(&adts);
        frame.extend_from_slice(raw);
        let pts = relative_timestamp(timestamp_ms, start_timestamp_ms);
        self.write_pes(AUDIO_PID, 0xc0, pts, pts, &frame);
        Ok(())
    }

    fn write_pat(&mut self) {
        let mut section = vec![
            0x00,
            0xb0,
            0x0d,
            0x00,
            0x01,
            0xc1,
            0x00,
            0x00,
            0x00,
            0x01,
            0xe0 | 0x10,
            0x00,
        ];
        let crc = mpeg_crc32(&section);
        section.extend_from_slice(&crc.to_be_bytes());
        self.write_psi(0, &section);
    }

    fn write_pmt(&mut self) {
        let mut section = vec![
            0x02,
            0xb0,
            0x17,
            0x00,
            0x01,
            0xc1,
            0x00,
            0x00,
            0xe0 | 0x01,
            0x01,
            0xf0,
            0x00,
            0x1b,
            0xe0 | 0x01,
            0x01,
            0xf0,
            0x00,
            0x0f,
            0xe0 | 0x01,
            0x02,
            0xf0,
            0x00,
        ];
        let crc = mpeg_crc32(&section);
        section.extend_from_slice(&crc.to_be_bytes());
        self.write_psi(PMT_PID, &section);
    }

    fn write_psi(&mut self, pid: u16, section: &[u8]) {
        let mut payload = Vec::with_capacity(section.len() + 1);
        payload.push(0);
        payload.extend_from_slice(section);
        self.write_payload(pid, true, &payload);
    }

    fn write_pes(&mut self, pid: u16, stream_id: u8, pts: u64, dts: u64, payload: &[u8]) {
        let mut packet_bytes = Vec::with_capacity(payload.len() + 19);
        packet_bytes.extend_from_slice(&[0, 0, 1, stream_id]);
        let packet_length = if stream_id == 0xe0 {
            0
        } else {
            u16::try_from(payload.len().saturating_add(13)).unwrap_or(0)
        };
        packet_bytes.extend_from_slice(&packet_length.to_be_bytes());
        packet_bytes.extend_from_slice(&[0x80, 0x80, 5]);
        packet_bytes.extend_from_slice(&encode_pts(pts, false));
        packet_bytes.extend_from_slice(payload);
        let _ = dts;
        self.write_payload(pid, true, &packet_bytes);
    }

    fn write_payload(&mut self, pid: u16, payload_unit_start: bool, payload: &[u8]) {
        let mut continuity = *self.continuity.entry(pid).or_default();
        let mut offset = 0;
        while offset < payload.len() {
            let remaining = payload.len() - offset;
            let payload_bytes = remaining.min(184);
            let adaptation = payload_bytes < 184;
            let mut packet = [0xff_u8; TS_PACKET_BYTES];
            packet[0] = 0x47;
            packet[1] = u8::try_from((pid >> 8) & 0x1f).expect("PID bits fit")
                | u8::from(payload_unit_start && offset == 0) << 6;
            packet[2] = u8::try_from(pid & 0xff).expect("PID bits fit");
            packet[3] = (continuity & 0x0f) | if adaptation { 0x30 } else { 0x10 };
            continuity = continuity.wrapping_add(1) & 0x0f;
            if adaptation {
                let adaptation_length = 183 - payload_bytes;
                packet[4] = u8::try_from(adaptation_length).expect("adaptation length fits");
                packet[5] = 0;
                let payload_start = 5 + adaptation_length;
                packet[payload_start..payload_start + payload_bytes]
                    .copy_from_slice(&payload[offset..offset + payload_bytes]);
            } else {
                packet[4..4 + payload_bytes]
                    .copy_from_slice(&payload[offset..offset + payload_bytes]);
            }
            self.bytes.extend_from_slice(&packet);
            offset += payload_bytes;
        }
        self.continuity.insert(pid, continuity);
    }

    pub(super) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

fn append_length_prefixed_nalus(
    output: &mut Vec<u8>,
    bytes: &[u8],
    length_size: usize,
) -> Result<(), ()> {
    let mut cursor = 0;
    while cursor < bytes.len() {
        let end = cursor.checked_add(length_size).ok_or(())?;
        let length_bytes = bytes.get(cursor..end).ok_or(())?;
        let length = length_bytes
            .iter()
            .fold(0_usize, |value, byte| (value << 8) | usize::from(*byte));
        cursor = end;
        let end = cursor.checked_add(length).ok_or(())?;
        let nalu = bytes.get(cursor..end).ok_or(())?;
        if !nalu.is_empty() {
            output.extend_from_slice(&[0, 0, 0, 1]);
            output.extend_from_slice(nalu);
        }
        cursor = end;
    }
    Ok(())
}

fn relative_timestamp(timestamp_ms: u32, start_timestamp_ms: u32) -> u64 {
    90_000_u64.saturating_add(u64::from(timestamp_ms.saturating_sub(start_timestamp_ms)) * 90)
}

fn relative_timestamp_with_composition(
    timestamp_ms: u32,
    composition_time: i32,
    start_timestamp_ms: u32,
) -> u64 {
    let timestamp = i64::from(timestamp_ms) + i64::from(composition_time);
    let start = i64::from(start_timestamp_ms);
    90_000_u64.saturating_add(u64::try_from((timestamp - start).max(0)).unwrap_or(0) * 90)
}

fn signed_24(bytes: &[u8]) -> i32 {
    let value = (i32::from(bytes[0]) << 16) | (i32::from(bytes[1]) << 8) | i32::from(bytes[2]);
    if value & 0x80_0000 != 0 {
        value | !0x00ff_ffff
    } else {
        value
    }
}

fn encode_pts(value: u64, marker: bool) -> [u8; 5] {
    let value = value & 0x1fff_fffff;
    [
        (u8::from(marker) << 4)
            | u8::try_from(((value >> 30) & 0x07) << 1).expect("PTS bits fit")
            | 1,
        u8::try_from((value >> 22) & 0xff).expect("PTS bits fit"),
        u8::try_from(((value >> 15) & 0x7f) << 1).expect("PTS bits fit") | 1,
        u8::try_from((value >> 7) & 0xff).expect("PTS bits fit"),
        u8::try_from((value & 0x7f) << 1).expect("PTS bits fit") | 1,
    ]
}

fn mpeg_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff;
    for byte in bytes {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}
