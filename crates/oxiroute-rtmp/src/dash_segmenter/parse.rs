#[allow(clippy::wildcard_imports)]
use super::*;

#[cfg(feature = "fuzzing")]
pub(crate) fn fuzz_media_configuration(payload: &[u8]) {
    let _ = parse_avc_config(payload);
    let _ = parse_aac_config(payload);
}

pub(super) fn parse_avc_config(payload: &[u8]) -> Option<AvcConfig> {
    let parsed = parse_avc_configuration(payload)?;
    if parsed.nal_length_size != 4
        || parsed.sps.is_empty()
        || parsed.sps.len() > MAX_CODEC_PARAMETER_SETS
        || parsed.pps.is_empty()
        || parsed.pps.len() > MAX_CODEC_PARAMETER_SETS
        || !parsed.trailing.is_empty()
    {
        return None;
    }
    if parsed.sps.iter().any(|parameter_set| {
        parameter_set.is_empty()
            || parameter_set.len() > MAX_CODEC_PARAMETER_SET_BYTES
            || parameter_set[0] & 0x1f != 7
    }) || parsed.pps.iter().any(|parameter_set| {
        parameter_set.is_empty()
            || parameter_set.len() > MAX_CODEC_PARAMETER_SET_BYTES
            || parameter_set[0] & 0x1f != 8
    }) {
        return None;
    }
    Some(AvcConfig {
        profile: parsed.profile,
        compatibility: parsed.compatibility,
        level: parsed.level,
        configuration_record: parsed.configuration_record.to_vec(),
    })
}

pub(super) fn parse_aac_config(payload: &[u8]) -> Option<AacConfig> {
    let parsed = parse_aac_configuration(payload)?;
    let sample_rate = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ]
    .get(usize::from(parsed.sample_rate_index))
    .copied()?;
    if parsed.audio_object_type != 2
        || parsed.channel_configuration == 0
        || parsed.channel_configuration > 6
    {
        return None;
    }
    let mut audio_specific_config = Vec::with_capacity(
        parsed
            .audio_specific_config
            .len()
            .saturating_add(parsed.trailing.len()),
    );
    audio_specific_config.extend_from_slice(parsed.audio_specific_config);
    audio_specific_config.extend_from_slice(parsed.trailing);
    Some(AacConfig {
        audio_object_type: parsed.audio_object_type,
        sample_rate,
        channel_configuration: parsed.channel_configuration,
        audio_specific_config,
    })
}

pub(super) fn parse_video_sample(payload: &[u8]) -> Option<(Vec<u8>, i32)> {
    if payload.len() < 9 || payload[0] & 0x0f != 7 || payload[1] != 1 {
        return None;
    }
    let composition_time_ms = signed_24(&payload[2..5]);
    let mut cursor = 5;
    let mut data = Vec::with_capacity(payload.len() - 5);
    while cursor < payload.len() {
        let end = cursor.checked_add(4)?;
        let length = usize::try_from(u32::from_be_bytes([
            *payload.get(cursor)?,
            *payload.get(cursor + 1)?,
            *payload.get(cursor + 2)?,
            *payload.get(cursor + 3)?,
        ]))
        .ok()?;
        cursor = end;
        if length == 0 {
            return None;
        }
        let end = cursor.checked_add(length)?;
        if payload.get(cursor..end)?.is_empty() {
            return None;
        }
        data.extend_from_slice(&payload[cursor - 4..end]);
        cursor = end;
    }
    (!data.is_empty()).then_some((data, composition_time_ms))
}

pub(super) fn parse_audio_sample(payload: &[u8]) -> Option<Vec<u8>> {
    (payload.len() > 2 && payload[0] >> 4 == 10 && payload[1] == 1)
        .then(|| payload[2..].to_vec())
        .filter(|data| !data.is_empty())
}

fn signed_24(bytes: &[u8]) -> i32 {
    let value = (i32::from(bytes[0]) << 16) | (i32::from(bytes[1]) << 8) | i32::from(bytes[2]);
    if value & 0x80_0000 != 0 {
        value | !0x00ff_ffff
    } else {
        value
    }
}
