#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn make_initialization(
    video: &AvcConfig,
    audio: &AacConfig,
    width: u16,
    height: u16,
) -> Vec<u8> {
    let mut output = Vec::new();
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"iso6");
    ftyp.extend_from_slice(&1_u32.to_be_bytes());
    ftyp.extend_from_slice(b"iso6");
    ftyp.extend_from_slice(b"dash");
    ftyp.extend_from_slice(b"mp41");
    append_box(&mut output, *b"ftyp", &ftyp);
    let mut moov = Vec::new();
    append_full_box(&mut moov, *b"mvhd", 0, &movie_header_payload());
    append_box(&mut moov, *b"trak", &video_track(video, width, height));
    append_box(&mut moov, *b"trak", &audio_track(audio));
    let mut mvex = Vec::new();
    append_full_box(&mut mvex, *b"trex", 0, &trex_payload(1));
    append_full_box(&mut mvex, *b"trex", 0, &trex_payload(2));
    append_box(&mut moov, *b"mvex", &mvex);
    append_box(&mut output, *b"moov", &moov);
    output
}

pub(super) fn make_fragment(
    video: &AvcConfig,
    audio: &AacConfig,
    segment: &DashSegmentBuilder,
    sequence: u32,
) -> Vec<u8> {
    let video_bytes = segment
        .video
        .iter()
        .map(|sample| sample.data.len())
        .sum::<usize>();
    let audio_bytes = segment
        .audio
        .iter()
        .map(|sample| sample.data.len())
        .sum::<usize>();
    let empty_video = build_moof(sequence, video, audio, segment, 0, 0);
    let video_offset = i32::try_from(empty_video.len().saturating_add(8)).unwrap_or(i32::MAX);
    let audio_offset = video_offset.saturating_add(i32::try_from(video_bytes).unwrap_or(i32::MAX));
    let moof = build_moof(sequence, video, audio, segment, video_offset, audio_offset);
    let mut output = moof;
    let mut mdat = Vec::with_capacity(video_bytes.saturating_add(audio_bytes));
    for sample in &segment.video {
        mdat.extend_from_slice(&sample.data);
    }
    for sample in &segment.audio {
        mdat.extend_from_slice(&sample.data);
    }
    append_box(&mut output, *b"mdat", &mdat);
    output
}

fn build_moof(
    sequence: u32,
    _video: &AvcConfig,
    audio: &AacConfig,
    segment: &DashSegmentBuilder,
    video_offset: i32,
    audio_offset: i32,
) -> Vec<u8> {
    let mut moof = Vec::new();
    let mut mfhd = Vec::new();
    mfhd.extend_from_slice(&sequence.to_be_bytes());
    append_full_box(&mut moof, *b"mfhd", 0, &mfhd);
    append_box(&mut moof, *b"traf", &video_traf(segment, video_offset));
    append_box(
        &mut moof,
        *b"traf",
        &audio_traf(segment, audio, audio_offset),
    );
    let mut output = Vec::with_capacity(moof.len() + 8);
    append_box(&mut output, *b"moof", &moof);
    output
}

fn video_traf(segment: &DashSegmentBuilder, data_offset: i32) -> Vec<u8> {
    let mut traf = Vec::new();
    append_full_box(&mut traf, *b"tfhd", 0x0002_0000, &1_u32.to_be_bytes());
    let base_decode_time = segment
        .video
        .first()
        .map_or(0, |sample| u64::from(sample.timestamp_ms) * 90);
    append_full_box(
        &mut traf,
        *b"tfdt",
        0x0100_0000,
        &base_decode_time.to_be_bytes(),
    );
    let mut trun = Vec::new();
    trun.extend_from_slice(
        &u32::try_from(segment.video.len())
            .expect("DASH video sample count is bounded")
            .to_be_bytes(),
    );
    trun.extend_from_slice(&data_offset.to_be_bytes());
    for (index, sample) in segment.video.iter().enumerate() {
        trun.extend_from_slice(&video_sample_duration(&segment.video, index).to_be_bytes());
        trun.extend_from_slice(
            &u32::try_from(sample.data.len())
                .expect("DASH video sample size is bounded")
                .to_be_bytes(),
        );
        let flags: u32 = if sample.keyframe {
            0x0200_0000
        } else {
            0x0101_0000
        };
        trun.extend_from_slice(&flags.to_be_bytes());
        let composition_time = i32::try_from(
            i64::from(sample.composition_time_ms) * i64::from(VIDEO_TIMESCALE) / 1_000,
        )
        .unwrap_or_else(|_| {
            if sample.composition_time_ms.is_negative() {
                i32::MIN
            } else {
                i32::MAX
            }
        });
        trun.extend_from_slice(&composition_time.to_be_bytes());
    }
    append_full_box(&mut traf, *b"trun", 0x0100_0f01, &trun);
    traf
}

fn audio_traf(segment: &DashSegmentBuilder, audio: &AacConfig, data_offset: i32) -> Vec<u8> {
    let mut traf = Vec::new();
    append_full_box(&mut traf, *b"tfhd", 0x0002_0000, &2_u32.to_be_bytes());
    let base_decode_time = segment.audio.first().map_or(0, |sample| {
        u64::from(sample.timestamp_ms).saturating_mul(u64::from(audio.sample_rate)) / 1_000
    });
    append_full_box(
        &mut traf,
        *b"tfdt",
        0x0100_0000,
        &base_decode_time.to_be_bytes(),
    );
    let mut trun = Vec::new();
    trun.extend_from_slice(
        &u32::try_from(segment.audio.len())
            .expect("DASH audio sample count is bounded")
            .to_be_bytes(),
    );
    trun.extend_from_slice(&data_offset.to_be_bytes());
    for sample in &segment.audio {
        trun.extend_from_slice(&AAC_SAMPLES_PER_FRAME.to_be_bytes());
        trun.extend_from_slice(
            &u32::try_from(sample.data.len())
                .expect("DASH audio sample size is bounded")
                .to_be_bytes(),
        );
    }
    append_full_box(&mut traf, *b"trun", 0x0000_0301, &trun);
    traf
}

fn video_sample_duration(samples: &[DashVideoSample], index: usize) -> u32 {
    samples
        .get(index + 1)
        .map_or(DEFAULT_VIDEO_SAMPLE_DURATION, |next| {
            let current = samples[index].timestamp_ms;
            let elapsed = next.timestamp_ms.saturating_sub(current);
            (u64::from(elapsed) * u64::from(VIDEO_TIMESCALE) / 1_000)
                .try_into()
                .unwrap_or(u32::MAX)
                .max(1)
        })
}

fn movie_header_payload() -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&1_000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
    payload.extend_from_slice(&0x0100_u16.to_be_bytes());
    payload.extend_from_slice(&[0; 10]);
    payload.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0x4000_0000_u32.to_be_bytes());
    payload.extend_from_slice(&[0; 24]);
    payload.extend_from_slice(&3_u32.to_be_bytes());
    payload
}

fn trex_payload(track_id: u32) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&track_id.to_be_bytes());
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload
}

fn video_track(config: &AvcConfig, width: u16, height: u16) -> Vec<u8> {
    let mut trak = Vec::new();
    append_full_box(
        &mut trak,
        *b"tkhd",
        0x0000_0007,
        &track_header_payload(1, 0, width, height),
    );
    let mut mdia = Vec::new();
    append_full_box(
        &mut mdia,
        *b"mdhd",
        0,
        &media_header_payload(VIDEO_TIMESCALE),
    );
    append_full_box(
        &mut mdia,
        *b"hdlr",
        0,
        &handler_payload(*b"vide", "VideoHandler"),
    );
    let mut minf = Vec::new();
    append_full_box(&mut minf, *b"vmhd", 1, &[0; 8]);
    append_box(&mut minf, *b"dinf", &data_information());
    append_box(
        &mut minf,
        *b"stbl",
        &video_sample_table(config, width, height),
    );
    append_box(&mut mdia, *b"minf", &minf);
    append_box(&mut trak, *b"mdia", &mdia);
    trak
}

fn audio_track(config: &AacConfig) -> Vec<u8> {
    let mut trak = Vec::new();
    append_full_box(
        &mut trak,
        *b"tkhd",
        0x0000_0007,
        &track_header_payload(2, 0x0100, 0, 0),
    );
    let mut mdia = Vec::new();
    append_full_box(
        &mut mdia,
        *b"mdhd",
        0,
        &media_header_payload(config.sample_rate),
    );
    append_full_box(
        &mut mdia,
        *b"hdlr",
        0,
        &handler_payload(*b"soun", "SoundHandler"),
    );
    let mut minf = Vec::new();
    append_full_box(&mut minf, *b"smhd", 0, &[0; 4]);
    append_box(&mut minf, *b"dinf", &data_information());
    append_box(&mut minf, *b"stbl", &audio_sample_table(config));
    append_box(&mut mdia, *b"minf", &minf);
    append_box(&mut trak, *b"mdia", &mdia);
    trak
}

fn track_header_payload(track_id: u32, volume: u16, width: u16, height: u16) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&track_id.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&[0; 8]);
    payload.extend_from_slice(&0_i16.to_be_bytes());
    payload.extend_from_slice(&0_i16.to_be_bytes());
    payload.extend_from_slice(&volume.to_be_bytes());
    payload.extend_from_slice(&0_u16.to_be_bytes());
    payload.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0x4000_0000_u32.to_be_bytes());
    payload.extend_from_slice(&[0; 24]);
    payload.extend_from_slice(&(u32::from(width) << 16).to_be_bytes());
    payload.extend_from_slice(&(u32::from(height) << 16).to_be_bytes());
    payload
}

fn media_header_payload(timescale: u32) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&timescale.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0x55c4_u16.to_be_bytes());
    payload.extend_from_slice(&0_u16.to_be_bytes());
    payload
}

fn handler_payload(handler: [u8; 4], name: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&handler);
    payload.extend_from_slice(&[0; 12]);
    payload.extend_from_slice(name.as_bytes());
    payload.push(0);
    payload
}

fn data_information() -> Vec<u8> {
    let mut dref = Vec::new();
    dref.extend_from_slice(&1_u32.to_be_bytes());
    append_full_box(&mut dref, *b"url ", 1, &[]);
    let mut dinf = Vec::new();
    append_full_box(&mut dinf, *b"dref", 0, &dref);
    dinf
}

fn video_sample_table(config: &AvcConfig, width: u16, height: u16) -> Vec<u8> {
    let mut avc1 = Vec::new();
    avc1.extend_from_slice(&[0; 6]);
    avc1.extend_from_slice(&1_u16.to_be_bytes());
    avc1.extend_from_slice(&0_u16.to_be_bytes());
    avc1.extend_from_slice(&0_u16.to_be_bytes());
    avc1.extend_from_slice(&[0; 12]);
    avc1.extend_from_slice(&width.to_be_bytes());
    avc1.extend_from_slice(&height.to_be_bytes());
    avc1.extend_from_slice(&0x0048_0000_u32.to_be_bytes());
    avc1.extend_from_slice(&0x0048_0000_u32.to_be_bytes());
    avc1.extend_from_slice(&0_u32.to_be_bytes());
    avc1.extend_from_slice(&1_u16.to_be_bytes());
    avc1.extend_from_slice(&[0; 32]);
    avc1.extend_from_slice(&0x0018_u16.to_be_bytes());
    avc1.extend_from_slice(&0xffff_u16.to_be_bytes());
    append_box(&mut avc1, *b"avcC", &config.configuration_record);
    let mut stsd_payload = 1_u32.to_be_bytes().to_vec();
    append_box(&mut stsd_payload, *b"avc1", &avc1);
    let mut stbl = Vec::new();
    append_full_box(&mut stbl, *b"stsd", 0, &stsd_payload);
    append_full_box(&mut stbl, *b"stts", 0, &0_u32.to_be_bytes());
    append_full_box(&mut stbl, *b"stsc", 0, &0_u32.to_be_bytes());
    append_full_box(&mut stbl, *b"stsz", 0, &[0; 8]);
    append_full_box(&mut stbl, *b"stco", 0, &0_u32.to_be_bytes());
    stbl
}

fn audio_sample_table(config: &AacConfig) -> Vec<u8> {
    let mut mp4a = Vec::new();
    mp4a.extend_from_slice(&[0; 6]);
    mp4a.extend_from_slice(&1_u16.to_be_bytes());
    mp4a.extend_from_slice(&[0; 8]);
    mp4a.extend_from_slice(&u16::from(config.channel_configuration).to_be_bytes());
    mp4a.extend_from_slice(&16_u16.to_be_bytes());
    mp4a.extend_from_slice(&0_u16.to_be_bytes());
    mp4a.extend_from_slice(&0_u16.to_be_bytes());
    mp4a.extend_from_slice(&(config.sample_rate << 16).to_be_bytes());
    let decoder_config = descriptor(0x05, &config.audio_specific_config);
    let mut decoder = Vec::new();
    decoder.push(0x40);
    decoder.push(0x15);
    decoder.extend_from_slice(&[0; 3]);
    decoder.extend_from_slice(&[0; 4]);
    decoder.extend_from_slice(&[0; 4]);
    decoder.extend_from_slice(&decoder_config);
    let mut es = Vec::new();
    es.extend_from_slice(&1_u16.to_be_bytes());
    es.push(0);
    es.extend_from_slice(&descriptor(0x04, &decoder));
    es.extend_from_slice(&descriptor(0x06, &[2]));
    append_full_box(&mut mp4a, *b"esds", 0, &es);
    let mut stsd_payload = 1_u32.to_be_bytes().to_vec();
    append_box(&mut stsd_payload, *b"mp4a", &mp4a);
    let mut stbl = Vec::new();
    append_full_box(&mut stbl, *b"stsd", 0, &stsd_payload);
    append_full_box(&mut stbl, *b"stts", 0, &0_u32.to_be_bytes());
    append_full_box(&mut stbl, *b"stsc", 0, &0_u32.to_be_bytes());
    append_full_box(&mut stbl, *b"stsz", 0, &[0; 8]);
    append_full_box(&mut stbl, *b"stco", 0, &0_u32.to_be_bytes());
    stbl
}

fn descriptor(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(payload.len() + 2);
    output.push(tag);
    output.push(u8::try_from(payload.len()).expect("DASH descriptor is bounded"));
    output.extend_from_slice(payload);
    output
}

fn append_box(output: &mut Vec<u8>, kind: [u8; 4], payload: &[u8]) {
    let size = u32::try_from(payload.len().saturating_add(8)).expect("DASH box is bounded");
    output.extend_from_slice(&size.to_be_bytes());
    output.extend_from_slice(&kind);
    output.extend_from_slice(payload);
}

fn append_full_box(output: &mut Vec<u8>, kind: [u8; 4], version_flags: u32, payload: &[u8]) {
    let mut full_payload = Vec::with_capacity(payload.len() + 4);
    full_payload.extend_from_slice(&version_flags.to_be_bytes());
    full_payload.extend_from_slice(payload);
    append_box(output, kind, &full_payload);
}
