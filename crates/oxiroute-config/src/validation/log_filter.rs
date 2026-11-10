use crate::{ConfigDraft, ConfigError};

const LEVELS: [&str; 6] = ["off", "error", "warn", "info", "debug", "trace"];

pub(super) fn validate_log_filter(filter: Option<&mut String>) -> Result<(), ConfigError> {
    let Some(filter) = filter else {
        return Ok(());
    };
    let trimmed = filter.trim();
    if trimmed.len() > ConfigDraft::MAX_LOG_FILTER_BYTES {
        return Err(invalid("exceeds 1024 bytes"));
    }
    trimmed
        .split(',')
        .try_for_each(|directive| directive_error(directive.trim()))?;
    *filter = trimmed.to_owned();
    Ok(())
}

fn directive_error(directive: &str) -> Result<(), ConfigError> {
    let (target, level) = match directive.split_once('=') {
        Some((target, level)) => (target, Some(level)),
        None => (directive, None),
    };
    if directive.contains('/') {
        return Err(invalid("regex filters are not supported"));
    }
    if target.is_empty() && level.is_none() || directive.starts_with('=') {
        return Err(invalid("a directive must name a level or a module"));
    }
    if !target
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':'))
    {
        return Err(invalid("module paths use letters, digits, `_` and `::`"));
    }
    if level.is_some_and(|level| !LEVELS.contains(&level.to_ascii_lowercase().as_str())) {
        return Err(invalid(
            "levels are off, error, warn, info, debug and trace",
        ));
    }
    Ok(())
}

fn invalid(detail: &'static str) -> ConfigError {
    ConfigError::InvalidLogFilter { detail }
}
