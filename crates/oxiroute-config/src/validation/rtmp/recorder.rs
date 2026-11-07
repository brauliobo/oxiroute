#[allow(clippy::wildcard_imports)]
use super::*;

#[allow(clippy::too_many_lines)]
pub(super) fn validate_rtmp_recorder(
    service: &str,
    application: &str,
    recorder: &mut RtmpRecorder,
) -> Result<(), ConfigError> {
    let invalid = |field, detail| ConfigError::InvalidRtmpRecorderPolicy {
        service: service.into(),
        application: application.into(),
        recorder: recorder.name.clone(),
        field,
        detail,
    };
    normalize_recording_root(&mut recorder.root_directory)
        .map_err(|detail| invalid("root_directory", detail))?;
    validate_recording_suffix_template(&recorder.suffix_template)
        .map_err(|detail| invalid("suffix_template", detail))?;
    if !recorder.record_mask.audio && !recorder.record_mask.video {
        return Err(invalid("record_mask", "must enable audio or video"));
    }
    if recorder.record_mask.keyframes && !recorder.record_mask.video {
        return Err(invalid(
            "record_mask.keyframes",
            "requires record_mask.video = true",
        ));
    }
    if let crate::model::RtmpRecorderTimezone::Iana(name) = &recorder.timezone {
        let parsed = name.parse::<chrono_tz::Tz>();
        if name.len() > 64 || parsed.is_err() || name.eq_ignore_ascii_case("utc") {
            return Err(invalid(
                "timezone",
                "must be `utc` or an exact IANA timezone name of at most 64 bytes",
            ));
        }
    }
    validate_rtmp_recorder_limit(
        recorder.max_queue_messages,
        MAX_RECORDER_QUEUE_MESSAGES,
        "max_queue_messages",
        "must be between 1 and 65536",
        &invalid,
    )?;
    validate_rtmp_recorder_limit(
        recorder.max_queue_bytes,
        MAX_RECORDER_QUEUE_BYTES,
        "max_queue_bytes",
        "must be between 1 and 1073741824",
        &invalid,
    )?;
    validate_rtmp_recorder_limit(
        recorder.shutdown_timeout_ms,
        MAX_RECORDER_SHUTDOWN_TIMEOUT_MS,
        "shutdown_timeout_ms",
        "must be between 1 and 60000",
        &invalid,
    )?;
    if let Some(max_storage_bytes) = recorder.max_storage_bytes {
        validate_rtmp_recorder_limit(
            max_storage_bytes,
            MAX_RECORDER_STORAGE_BYTES,
            "max_storage_bytes",
            "must be null or between 1 and 1099511627776",
            &invalid,
        )?;
    }
    if let Some(max_storage_files) = recorder.max_storage_files {
        validate_rtmp_recorder_limit(
            max_storage_files,
            MAX_RECORDER_STORAGE_FILES,
            "max_storage_files",
            "must be null or between 1 and 1000000",
            &invalid,
        )?;
    }
    if let Some(max_size) = recorder.max_size {
        validate_rtmp_recorder_limit(
            max_size,
            MAX_RECORDER_FILE_BYTES,
            "max_size",
            "must be null or between 1 and 1099511627776",
            &invalid,
        )?;
    }
    if let Some(max_frames) = recorder.max_frames {
        validate_rtmp_recorder_limit(
            max_frames,
            MAX_RECORDER_FRAME_COUNT,
            "max_frames",
            "must be null or between 1 and 1000000000",
            &invalid,
        )?;
    }
    validate_rtmp_recorder_limit(
        recorder.max_active_recorders,
        MAX_RECORDER_ACTIVE_RECORDERS,
        "max_active_recorders",
        "must be between 1 and 256",
        &invalid,
    )?;
    if recorder
        .rotation_interval_ms
        .is_some_and(|interval| interval == 0 || interval > MAX_RECORDER_ROTATION_INTERVAL_MS)
    {
        return Err(invalid(
            "rotation_interval_ms",
            "must be null or between 1 and 2147483647",
        ));
    }
    if recorder
        .max_storage_bytes
        .is_some_and(|maximum| recorder.max_queue_bytes > maximum)
    {
        return Err(ConfigError::RtmpRecorderQueueExceedsStorage {
            service: service.into(),
            application: application.into(),
            recorder: recorder.name.clone(),
        });
    }
    Ok(())
}

fn validate_rtmp_recorder_limit(
    value: u64,
    maximum: u64,
    field: &'static str,
    detail: &'static str,
    invalid: &impl Fn(&'static str, &'static str) -> ConfigError,
) -> Result<(), ConfigError> {
    if value == 0 || value > maximum {
        return Err(invalid(field, detail));
    }
    Ok(())
}
