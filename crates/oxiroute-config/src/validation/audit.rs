use crate::{Audit, ConfigError, lexical::validate_directory_path};

pub(super) fn validate_audit(audit: &mut Option<Audit>) -> Result<(), ConfigError> {
    let Some(settings) = audit else {
        return Ok(());
    };
    if let Some(dir) = &settings.dir {
        validate_directory_path("audit", "audit", "dir", dir)?;
    }
    bounded(
        "/audit/max_records",
        settings.max_records,
        Audit::MAX_RECORDS,
    )?;
    let record = bounded(
        "/audit/max_record_bytes",
        settings.max_record_bytes,
        Audit::MAX_RECORD_BYTES,
    )?;
    let file = bounded(
        "/audit/max_file_bytes",
        settings.max_file_bytes,
        Audit::MAX_FILE_BYTES,
    )?;
    let total = bounded(
        "/audit/max_total_bytes",
        settings.max_total_bytes,
        Audit::MAX_TOTAL_BYTES,
    )?;
    bounded(
        "/audit/max_rotated_files",
        settings.max_rotated_files,
        Audit::MAX_ROTATED_FILES,
    )?;
    let (record, file, total) = (
        record.unwrap_or(Audit::DEFAULT_MAX_RECORD_BYTES),
        file.unwrap_or(Audit::DEFAULT_MAX_FILE_BYTES),
        total.unwrap_or(Audit::DEFAULT_MAX_TOTAL_BYTES),
    );
    if record > file {
        return Err(invalid(
            "/audit/max_record_bytes",
            "must not exceed max_file_bytes",
        ));
    }
    if total < file {
        return Err(invalid(
            "/audit/max_total_bytes",
            "must not be below max_file_bytes",
        ));
    }
    if *settings == Audit::default() {
        *audit = None;
    }
    Ok(())
}

fn bounded(
    pointer: &'static str,
    value: Option<u64>,
    maximum: u64,
) -> Result<Option<u64>, ConfigError> {
    match value {
        Some(value) if !(1..=maximum).contains(&value) => {
            Err(invalid(pointer, format!("must be between 1 and {maximum}")))
        }
        _ => Ok(value),
    }
}

fn invalid(pointer: &'static str, detail: impl Into<String>) -> ConfigError {
    ConfigError::InvalidAudit {
        pointer,
        detail: detail.into(),
    }
}
