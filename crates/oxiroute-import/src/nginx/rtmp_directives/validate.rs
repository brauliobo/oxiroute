#[allow(clippy::wildcard_imports)]
use super::*;

#[must_use]
pub fn directive_specs() -> &'static [DirectiveSpec] {
    &DIRECTIVES
}

#[must_use]
pub fn directive_compatibility_report() -> DirectiveCompatibilityReport {
    let mut directive_status = DirectiveStatusCounts::default();
    let mut form_status = DirectiveStatusCounts::default();

    for spec in &DIRECTIVES {
        increment_status(&mut directive_status, spec.compatibility_status());
        for form in spec.runtime_forms {
            increment_status(&mut form_status, form.status);
        }
    }

    DirectiveCompatibilityReport {
        entries: &DIRECTIVES,
        directive_status,
        form_status,
    }
}

fn increment_status(counts: &mut DirectiveStatusCounts, status: DirectiveStatus) {
    match status {
        DirectiveStatus::Enforced => counts.enforced += 1,
        DirectiveStatus::Partial => counts.partial += 1,
        DirectiveStatus::DisableOnly => counts.disable_only += 1,
        DirectiveStatus::ParsedOnly => counts.parsed_only += 1,
        DirectiveStatus::SourceNoOp => counts.source_no_op += 1,
        DirectiveStatus::SourceBug => counts.source_bug += 1,
        DirectiveStatus::Deprecated => counts.deprecated += 1,
        DirectiveStatus::PlatformLimited => counts.platform_limited += 1,
    }
}

/// Validates one directive after nginx tokenization.
///
/// # Errors
///
/// Returns an error for unknown keys, invalid contexts, arity, or values.
pub fn validate_directive(
    name: &str,
    context: DirectiveContext,
    args: &[&str],
) -> Result<&'static DirectiveSpec, DirectiveError> {
    let spec = DIRECTIVES
        .iter()
        .find(|spec| spec.name == name)
        .ok_or_else(|| DirectiveError::UnknownDirective(name.to_owned()))?;

    if !spec.contexts.contains(&context) {
        return Err(DirectiveError::InvalidContext {
            name: spec.name,
            context,
        });
    }
    if args.len() < usize::from(spec.min_args)
        || spec
            .max_args
            .is_some_and(|max| args.len() > usize::from(max))
    {
        return Err(DirectiveError::InvalidArity {
            name: spec.name,
            expected: expected_arity(spec),
            actual: args.len(),
        });
    }

    validate_value(spec, args)?;
    Ok(spec)
}

fn expected_arity(spec: &DirectiveSpec) -> String {
    match spec.max_args {
        Some(max) if max == spec.min_args => spec.min_args.to_string(),
        Some(max) => format!("{}..={max}", spec.min_args),
        None => format!("at least {}", spec.min_args),
    }
}

fn validate_value(spec: &DirectiveSpec, args: &[&str]) -> Result<(), DirectiveError> {
    let valid = match spec.value_kind {
        ValueKind::AccessLog => args[0] != "off" || args.len() == 1,
        ValueKind::AccessRule => valid_access_rule(args),
        ValueKind::Bitmask(values) => args.iter().all(|arg| values.contains(arg)),
        ValueKind::Block => true,
        ValueKind::Command | ValueKind::HlsVariant | ValueKind::NamedBlock => !args[0].is_empty(),
        ValueKind::Duration => valid_duration(args[0]),
        ValueKind::DurationOrOff => args[0] == "off" || valid_duration(args[0]),
        ValueKind::Enum(values) => values.contains(&args[0]),
        ValueKind::Flag => matches!(args[0], "on" | "off"),
        ValueKind::Integer => args[0].parse::<u64>().is_ok(),
        ValueKind::Listen => valid_listen(args),
        ValueKind::LogFormat | ValueKind::Path | ValueKind::Strings => {
            args.iter().all(|arg| !arg.is_empty())
        }
        ValueKind::RelayTarget(kind) => valid_relay_target(kind, args),
        ValueKind::Signal => valid_signal(args[0]),
        ValueKind::Size => valid_size(args[0]),
        ValueKind::Url => args[0].starts_with("http://") && args[0].len() > "http://".len(),
    };

    if valid {
        Ok(())
    } else {
        Err(DirectiveError::InvalidValue {
            name: spec.name,
            detail: args.join(" "),
        })
    }
}

fn valid_access_rule(args: &[&str]) -> bool {
    match args {
        [target] => valid_access_target(target),
        [operation, target] => {
            matches!(*operation, "publish" | "play") && valid_access_target(target)
        }
        _ => false,
    }
}

fn valid_access_target(target: &str) -> bool {
    if target == "all" {
        return true;
    }
    let Some((address, prefix)) = target.split_once('/') else {
        return target.parse::<IpAddr>().is_ok();
    };
    let Ok(address) = address.parse::<IpAddr>() else {
        return false;
    };
    let Ok(prefix) = prefix.parse::<u8>() else {
        return false;
    };
    prefix <= if address.is_ipv4() { 32 } else { 128 }
}

fn valid_listen(args: &[&str]) -> bool {
    if args[0].is_empty() {
        return false;
    }
    let Some(option) = args.get(1) else {
        return true;
    };

    if matches!(
        *option,
        "bind"
            | "ipv6only=on"
            | "ipv6only=off"
            | "so_keepalive=on"
            | "so_keepalive=off"
            | "proxy_protocol"
    ) {
        return true;
    }

    option.strip_prefix("so_keepalive=").is_some_and(|value| {
        let parts: Vec<_> = value.split(':').collect();
        parts.len() == 3 && parts.iter().all(|part| valid_seconds_duration(part))
    })
}

fn valid_relay_target(kind: RelayKind, args: &[&str]) -> bool {
    if args[0].is_empty() {
        return false;
    }

    let mut has_name = false;
    let mut is_static = false;
    for option in &args[1..] {
        let (key, value) = option
            .split_once('=')
            .map_or((*option, None), |(key, value)| (key, Some(value)));

        let valid = match key {
            "app" | "tcUrl" | "pageUrl" | "swfUrl" | "flashVer" | "playPath" => value.is_some(),
            "name" => {
                has_name = value.is_some_and(|value| !value.is_empty());
                has_name
            }
            "live" | "start" | "stop" => value.is_none_or(|value| value.parse::<u64>().is_ok()),
            "static" => {
                is_static = true;
                value.is_none_or(|value| value.parse::<u64>().is_ok())
            }
            _ => false,
        };
        if !valid {
            return false;
        }
    }

    match kind {
        RelayKind::Push => !is_static,
        RelayKind::Pull => !is_static || has_name,
    }
}

fn valid_signal(value: &str) -> bool {
    const SIGNALS: &[&str] = &[
        "HUP", "INT", "QUIT", "ILL", "ABRT", "FPE", "KILL", "SEGV", "PIPE", "ALRM", "TERM", "USR1",
        "USR2", "CHLD", "CONT", "STOP", "TSTP", "TTIN", "TTOU",
    ];

    value.parse::<u32>().is_ok() || SIGNALS.contains(&value)
}

pub(super) fn valid_size(value: &str) -> bool {
    let (number, suffix) = value
        .char_indices()
        .last()
        .filter(|(_, last)| last.is_ascii_alphabetic())
        .map_or((value, None), |(index, last)| {
            (&value[..index], Some(last.to_ascii_lowercase()))
        });

    !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && suffix.is_none_or(|suffix| matches!(suffix, 'k' | 'm' | 'g'))
}

pub(super) fn valid_duration(value: &str) -> bool {
    valid_nginx_time(value, false)
}

pub(super) fn valid_seconds_duration(value: &str) -> bool {
    valid_nginx_time(value, true)
}

fn valid_nginx_time(value: &str, seconds_resolution: bool) -> bool {
    if value.is_empty() || !value.is_ascii() {
        return false;
    }

    let bytes = value.as_bytes();
    let mut index = 0;
    let mut previous_unit = if seconds_resolution { 0 } else { 2 };
    while index < bytes.len() {
        let number_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if number_start == index {
            return false;
        }
        if index == bytes.len() {
            return true;
        }

        let unit = if bytes[index..].starts_with(b"ms") {
            if seconds_resolution {
                return false;
            }
            index += 2;
            8
        } else {
            let unit = match bytes[index] {
                b'y' => 1,
                b'M' => 2,
                b'w' => 3,
                b'd' => 4,
                b'h' => 5,
                b'm' => 6,
                b's' => 7,
                _ => return false,
            };
            index += 1;
            unit
        };
        if unit <= previous_unit {
            return false;
        }
        previous_unit = unit;
    }

    true
}
