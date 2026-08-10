pub(crate) struct AacConfiguration<'a> {
    pub(crate) audio_object_type: u8,
    pub(crate) sample_rate_index: u8,
    pub(crate) channel_configuration: u8,
    pub(crate) audio_specific_config: &'a [u8],
    pub(crate) trailing: &'a [u8],
}

pub(crate) struct AvcConfiguration<'a> {
    pub(crate) profile: u8,
    pub(crate) compatibility: u8,
    pub(crate) level: u8,
    pub(crate) nal_length_size: usize,
    pub(crate) sps: Vec<&'a [u8]>,
    pub(crate) pps: Vec<&'a [u8]>,
    pub(crate) configuration_record: &'a [u8],
    pub(crate) trailing: &'a [u8],
}

pub(crate) fn parse_aac_configuration(payload: &[u8]) -> Option<AacConfiguration<'_>> {
    if payload.len() < 4 || payload[0] >> 4 != 10 || payload[1] != 0 {
        return None;
    }
    let first = payload[2];
    let second = payload[3];
    Some(AacConfiguration {
        audio_object_type: first >> 3,
        sample_rate_index: ((first & 0x07) << 1) | (second >> 7),
        channel_configuration: (second >> 3) & 0x0f,
        audio_specific_config: &payload[2..4],
        trailing: &payload[4..],
    })
}

pub(crate) fn parse_avc_configuration(payload: &[u8]) -> Option<AvcConfiguration<'_>> {
    if payload.len() < 11 || payload[0] & 0x0f != 7 || payload[1] != 0 || payload[5] != 1 {
        return None;
    }
    let mut cursor = 11;
    let sps_count = usize::from(payload[10] & 0x1f);
    let sps = parse_parameter_sets(payload, &mut cursor, sps_count)?;
    let pps_count = usize::from(*payload.get(cursor)?);
    cursor = cursor.checked_add(1)?;
    let pps = parse_parameter_sets(payload, &mut cursor, pps_count)?;
    Some(AvcConfiguration {
        profile: payload[6],
        compatibility: payload[7],
        level: payload[8],
        nal_length_size: usize::from(payload[9] & 0x03) + 1,
        sps,
        pps,
        configuration_record: &payload[5..cursor],
        trailing: &payload[cursor..],
    })
}

fn parse_parameter_sets<'a>(
    payload: &'a [u8],
    cursor: &mut usize,
    count: usize,
) -> Option<Vec<&'a [u8]>> {
    let mut parameter_sets = Vec::with_capacity(count);
    for _ in 0..count {
        let length_end = cursor.checked_add(2)?;
        let length = usize::from(u16::from_be_bytes(
            payload.get(*cursor..length_end)?.try_into().ok()?,
        ));
        let end = length_end.checked_add(length)?;
        parameter_sets.push(payload.get(length_end..end)?);
        *cursor = end;
    }
    Some(parameter_sets)
}
