#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn load_manifest(
    store: &MediaStore,
    directory: &Path,
) -> Result<(VecDeque<PublishedSegment>, u64), MediaStoreError> {
    let manifest_path = directory.join("manifest.mpd");
    let bytes = match store.read_relative(&manifest_path, store.limits().max_file_bytes) {
        Ok(bytes) => bytes,
        Err(MediaStoreError::NotFound) => return Ok((VecDeque::new(), 0)),
        Err(error) => return Err(error),
    };
    let text = str::from_utf8(&bytes).map_err(|_| MediaStoreError::ManifestMalformed)?;
    if !text.starts_with("<?xml")
        || !text.contains("<MPD ")
        || !text.contains("<SegmentList ")
        || !text.contains("</SegmentList>")
        || !text.contains("</MPD>")
    {
        return Err(MediaStoreError::ManifestMalformed);
    }
    let mut published = VecDeque::new();
    let mut cursor = 0;
    while let Some(start) = text[cursor..].find("<SegmentURL ") {
        let start = cursor + start;
        let end = text[start..]
            .find("/>")
            .map(|offset| start + offset)
            .ok_or(MediaStoreError::ManifestMalformed)?;
        let tag = &text[start..end];
        let media = xml_attribute(tag, "media").ok_or(MediaStoreError::ManifestMalformed)?;
        let duration_ms = xml_attribute(tag, "d")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|duration| *duration > 0)
            .ok_or(MediaStoreError::ManifestMalformed)?;
        let start_timestamp_ms = xml_attribute(tag, "t")
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or(MediaStoreError::ManifestMalformed)?;
        if !valid_segment_filename(media) {
            return Err(MediaStoreError::ManifestMalformed);
        }
        let path = directory.join(media);
        store
            .read_relative(&path, store.limits().max_file_bytes)
            .map_err(|_| MediaStoreError::ManifestMalformed)?;
        let sequence = sequence_from_filename(media).ok_or(MediaStoreError::ManifestMalformed)?;
        if published
            .iter()
            .any(|segment: &PublishedSegment| segment.sequence == sequence)
        {
            return Err(MediaStoreError::ManifestMalformed);
        }
        published.push_back(PublishedSegment {
            sequence,
            start_timestamp_ms,
            duration_ms,
            path,
        });
        cursor = end + 2;
    }
    if published.is_empty() {
        return Err(MediaStoreError::ManifestMalformed);
    }
    let next_sequence = published
        .iter()
        .map(|segment| segment.sequence)
        .max()
        .and_then(|sequence| sequence.checked_add(1))
        .unwrap_or(0);
    Ok((published, next_sequence))
}

pub(super) fn render_mpd(
    segments: &VecDeque<PublishedSegment>,
    video: &AvcConfig,
    audio: &AacConfig,
    width: u16,
    height: u16,
    playlist_length: Duration,
    end_list: bool,
) -> String {
    let total_duration_ms = segments
        .back()
        .map_or(0, |segment| {
            u64::from(segment.start_timestamp_ms).saturating_add(segment.duration_ms)
        })
        .saturating_sub(u64::from(
            segments
                .front()
                .map_or(0, |segment| segment.start_timestamp_ms),
        ));
    let mut output = String::with_capacity(2_048 + segments.len() * 128);
    output.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        output,
        "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"{}\" profiles=\"urn:mpeg:dash:profile:isoff-live:2011\" minBufferTime=\"PT1S\" mediaPresentationDuration=\"{}\" timeShiftBufferDepth=\"{}\">",
        if end_list { "static" } else { "dynamic" },
        iso_duration(total_duration_ms),
        iso_duration(duration_millis(playlist_length)),
    );
    output.push_str("<Period id=\"0\" start=\"PT0S\">\n");
    let _ = writeln!(
        output,
        "<AdaptationSet id=\"0\" contentType=\"video\" mimeType=\"video/mp4\" codecs=\"{},mp4a.40.2\">",
        video.codec_string()
    );
    let _ = writeln!(
        output,
        "<Representation id=\"avc-aac\" bandwidth=\"1000000\"{}{}>",
        if width > 0 {
            format!(" width=\"{width}\"")
        } else {
            String::new()
        },
        if height > 0 {
            format!(" height=\"{height}\"")
        } else {
            String::new()
        },
    );
    let _ = writeln!(
        output,
        "<SegmentList timescale=\"1000\" presentationTimeOffset=\"0\"><Initialization sourceURL=\"init.mp4\"/>"
    );
    for segment in segments {
        let filename = segment
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("invalid.m4s");
        let _ = writeln!(
            output,
            "<SegmentURL media=\"{filename}\" t=\"{}\" d=\"{}\"/>",
            segment.start_timestamp_ms, segment.duration_ms
        );
    }
    output.push_str("</SegmentList></Representation></AdaptationSet>\n");
    let _ = writeln!(
        output,
        "<!-- AAC sample rate {} Hz, {} channels, object type {} -->",
        audio.sample_rate, audio.channel_configuration, audio.audio_object_type
    );
    output.push_str("</Period></MPD>\n");
    output
}

pub(super) fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn duration_millis_for_audio(sample_count: usize, sample_rate: u32) -> u64 {
    u64::try_from(sample_count)
        .unwrap_or(u64::MAX)
        .saturating_mul(u64::from(AAC_SAMPLES_PER_FRAME))
        .saturating_mul(1_000)
        / u64::from(sample_rate.max(1))
}

pub(super) fn segment_duration_ms(segment: &DashSegmentBuilder, sample_rate: u32) -> u64 {
    let audio_end = segment.audio.last().map_or(0, |sample| {
        u64::from(sample.timestamp_ms).saturating_add(duration_millis_for_audio(1, sample_rate))
    });
    u64::from(segment.last_timestamp_ms)
        .max(audio_end)
        .saturating_sub(u64::from(segment.start_timestamp_ms))
        .max(1)
}

pub(super) fn sum_durations(segments: &VecDeque<PublishedSegment>) -> u64 {
    segments.iter().fold(0, |total, segment| {
        total.saturating_add(segment.duration_ms)
    })
}

fn iso_duration(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    let remainder = milliseconds % 1_000;
    if remainder == 0 {
        format!("PT{seconds}S")
    } else {
        let mut fraction = format!("{remainder:03}");
        while fraction.ends_with('0') {
            fraction.pop();
        }
        format!("PT{seconds}.{fraction}S")
    }
}

fn xml_attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{name}=\"");
    let start = tag.find(&prefix)? + prefix.len();
    let end = start + tag[start..].find('"')?;
    Some(&tag[start..end])
}

pub(super) fn valid_segment_filename(value: &str) -> bool {
    value.len() <= MAX_DASH_FILENAME_BYTES
        && value.is_ascii()
        && Path::new(value)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("m4s"))
        && value.starts_with("seg-")
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && sequence_from_filename(value).is_some()
}

fn sequence_from_filename(value: &str) -> Option<u64> {
    value.strip_suffix(".m4s")?.rsplit('-').next()?.parse().ok()
}
