#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(super) enum Consumer {
    Hls,
    Dash,
}

pub(super) struct ConfigCase {
    pub(super) name: String,
    pub(super) payload: Vec<u8>,
    hls: bool,
    dash: bool,
}

impl ConfigCase {
    pub(super) fn accepted_by(&self, consumer: Consumer) -> bool {
        match consumer {
            Consumer::Hls => self.hls,
            Consumer::Dash => self.dash,
        }
    }
}

pub(super) fn avc_cases() -> Vec<ConfigCase> {
    let sps = vec![0x67, 0x42, 0, 0x1e];
    let pps = vec![0x68, 0xce];
    let canonical = avc_config(4, std::slice::from_ref(&sps), std::slice::from_ref(&pps), &[]);
    let mut cases = vec![config_case("canonical", canonical.clone(), true, true)];

    for length in 0..canonical.len() {
        cases.push(config_case(
            format!("canonical truncated at byte {length}"),
            canonical[..length].to_vec(),
            false,
            false,
        ));
    }

    let mut wrong_wrapper = canonical.clone();
    wrong_wrapper[0] = 0x16;
    cases.push(config_case("wrong AVC wrapper", wrong_wrapper, false, false));
    let mut wrong_packet_type = canonical.clone();
    wrong_packet_type[1] = 1;
    cases.push(config_case(
        "wrong AVC packet type",
        wrong_packet_type,
        false,
        false,
    ));
    let mut wrong_version = canonical;
    wrong_version[5] = 2;
    cases.push(config_case(
        "wrong AVC configuration version",
        wrong_version,
        false,
        false,
    ));

    for length_size in 1..=3 {
        cases.push(config_case(
            format!("{length_size}-byte NAL lengths"),
            avc_config(
                length_size,
                std::slice::from_ref(&sps),
                std::slice::from_ref(&pps),
                &[],
            ),
            true,
            false,
        ));
    }

    cases.push(config_case(
        "zero SPS entries",
        avc_config(4, &[], std::slice::from_ref(&pps), &[]),
        false,
        false,
    ));
    for count in [2, 4, 5] {
        cases.push(config_case(
            format!("{count} SPS entries"),
            avc_config(
                4,
                &vec![sps.clone(); count],
                std::slice::from_ref(&pps),
                &[],
            ),
            true,
            count <= 4,
        ));
    }
    cases.push(config_case(
        "empty SPS entry",
        avc_config(4, &[Vec::new()], std::slice::from_ref(&pps), &[]),
        true,
        false,
    ));
    cases.push(config_case(
        "mistyped SPS entry",
        avc_config(4, &[vec![0x68]], std::slice::from_ref(&pps), &[]),
        true,
        false,
    ));

    cases.push(config_case(
        "zero PPS entries",
        avc_config(4, std::slice::from_ref(&sps), &[], &[]),
        false,
        false,
    ));
    for count in [2, 4, 5] {
        cases.push(config_case(
            format!("{count} PPS entries"),
            avc_config(
                4,
                std::slice::from_ref(&sps),
                &vec![pps.clone(); count],
                &[],
            ),
            true,
            count <= 4,
        ));
    }
    cases.push(config_case(
        "empty PPS entry",
        avc_config(4, std::slice::from_ref(&sps), &[Vec::new()], &[]),
        true,
        false,
    ));
    cases.push(config_case(
        "mistyped PPS entry",
        avc_config(4, std::slice::from_ref(&sps), &[vec![0x67]], &[]),
        true,
        false,
    ));
    cases.push(config_case(
        "trailing AVC bytes",
        avc_config(
            4,
            std::slice::from_ref(&sps),
            std::slice::from_ref(&pps),
            &[0xaa],
        ),
        true,
        false,
    ));

    cases
}

pub(super) fn aac_cases() -> Vec<ConfigCase> {
    let canonical = aac_config(2, 4, 2, &[]);
    let mut cases = vec![config_case("canonical", canonical.clone(), true, true)];

    for length in 0..canonical.len() {
        cases.push(config_case(
            format!("canonical truncated at byte {length}"),
            canonical[..length].to_vec(),
            false,
            false,
        ));
    }

    let mut wrong_wrapper = canonical.clone();
    wrong_wrapper[0] = 0x9f;
    cases.push(config_case("wrong AAC wrapper", wrong_wrapper, false, false));
    let mut wrong_packet_type = canonical;
    wrong_packet_type[1] = 1;
    cases.push(config_case(
        "wrong AAC packet type",
        wrong_packet_type,
        false,
        false,
    ));

    for (object_type, hls, dash) in [
        (0, false, false),
        (1, true, false),
        (2, true, true),
        (3, true, false),
        (4, true, false),
        (5, false, false),
    ] {
        cases.push(config_case(
            format!("AAC object type {object_type}"),
            aac_config(object_type, 4, 2, &[]),
            hls,
            dash,
        ));
    }

    for (sample_rate_index, hls, dash) in [
        (0, false, true),
        (1, true, true),
        (12, true, true),
        (13, true, false),
        (14, true, false),
        (15, false, false),
    ] {
        cases.push(config_case(
            format!("AAC sample-rate index {sample_rate_index}"),
            aac_config(2, sample_rate_index, 2, &[]),
            hls,
            dash,
        ));
    }

    for (channel_configuration, accepted) in [(0, false), (1, true), (6, true), (7, false)] {
        cases.push(config_case(
            format!("AAC channel configuration {channel_configuration}"),
            aac_config(2, 4, channel_configuration, &[]),
            accepted,
            accepted,
        ));
    }
    cases.push(config_case(
        "trailing AAC bytes",
        aac_config(2, 4, 2, &[0xaa, 0x55]),
        true,
        true,
    ));

    cases
}

fn config_case(name: impl Into<String>, payload: Vec<u8>, hls: bool, dash: bool) -> ConfigCase {
    ConfigCase {
        name: name.into(),
        payload,
        hls,
        dash,
    }
}

fn avc_config(
    nal_length_size: u8,
    sps: &[Vec<u8>],
    pps: &[Vec<u8>],
    trailing: &[u8],
) -> Vec<u8> {
    let mut payload = vec![
        0x17,
        0,
        0,
        0,
        0,
        1,
        0x42,
        0,
        0x1e,
        0xfc | (nal_length_size - 1),
        0xe0 | u8::try_from(sps.len()).expect("bounded SPS count"),
    ];
    for parameter_set in sps {
        payload.extend_from_slice(
            &u16::try_from(parameter_set.len())
                .expect("bounded SPS length")
                .to_be_bytes(),
        );
        payload.extend_from_slice(parameter_set);
    }
    payload.push(u8::try_from(pps.len()).expect("bounded PPS count"));
    for parameter_set in pps {
        payload.extend_from_slice(
            &u16::try_from(parameter_set.len())
                .expect("bounded PPS length")
                .to_be_bytes(),
        );
        payload.extend_from_slice(parameter_set);
    }
    payload.extend_from_slice(trailing);
    payload
}

fn aac_config(
    object_type: u8,
    sample_rate_index: u8,
    channel_configuration: u8,
    trailing: &[u8],
) -> Vec<u8> {
    let first = (object_type << 3) | (sample_rate_index >> 1);
    let second = ((sample_rate_index & 1) << 7) | (channel_configuration << 3);
    let mut payload = vec![0xaf, 0, first, second];
    payload.extend_from_slice(trailing);
    payload
}
