fn parse_header(kind: SectionKind, directive: &Directive) -> Result<ParsedHeader, &'static str> {
    let arguments = directive.arguments.as_slice();
    match kind {
        SectionKind::Global if arguments.is_empty() => Ok(ParsedHeader {
            name: None,
            from: None,
        }),
        SectionKind::Global => Err("`global` takes no arguments"),
        SectionKind::Defaults => match arguments {
            [] => Ok(ParsedHeader {
                name: None,
                from: None,
            }),
            [name] if name.value != b"from" => Ok(ParsedHeader {
                name: Some((name.value.clone(), name.span)),
                from: None,
            }),
            [from, target] if from.value == b"from" => Ok(ParsedHeader {
                name: None,
                from: Some((target.value.clone(), target.span)),
            }),
            [name, from, target] if name.value != b"from" && from.value == b"from" => {
                Ok(ParsedHeader {
                    name: Some((name.value.clone(), name.span)),
                    from: Some((target.value.clone(), target.span)),
                })
            }
            _ => Err("expected `defaults [name] [from defaults_name]`"),
        },
        SectionKind::Frontend | SectionKind::Backend | SectionKind::Listen => match arguments {
            [name] if name.value != b"from" => Ok(ParsedHeader {
                name: Some((name.value.clone(), name.span)),
                from: None,
            }),
            [name, from, target] if name.value != b"from" && from.value == b"from" => {
                Ok(ParsedHeader {
                    name: Some((name.value.clone(), name.span)),
                    from: Some((target.value.clone(), target.span)),
                })
            }
            _ => Err("expected a name followed by optional `from defaults_name`"),
        },
        _ => Err("section is not supported"),
    }
}

fn effective_section(meta: &SectionMeta, header: &ParsedHeader) -> EffectiveSection {
    EffectiveSection {
        id: meta.id,
        declaration: OccurrenceId::SectionHeader(meta.id),
        name: header.name.as_ref().map(|(name, _)| name.clone()),
        span: meta.section.header.span,
    }
}

fn defaults_source(
    meta: &SectionMeta,
    target: SectionId,
    target_occurrence: OccurrenceId,
    target_span: Span,
    selection: DefaultsSelection,
    reference_span: Span,
) -> DefaultsSource {
    DefaultsSource {
        section: target,
        selection,
        provenance: Provenance::direct(
            OccurrenceId::SectionHeader(meta.id),
            meta.section.header.span,
        )
        .with_reference(
            reference_span,
            vec![ReferenceTarget {
                occurrence: target_occurrence,
                span: target_span,
            }],
        ),
    }
}

fn section_directive_id(section: SectionId, directive_ordinal: usize) -> OccurrenceId {
    OccurrenceId::SectionDirective {
        section,
        directive_ordinal,
    }
}

fn exactly_one_argument(directive: &Directive) -> Option<&super::Word> {
    let [argument] = directive.arguments.as_slice() else {
        return None;
    };
    Some(argument)
}

fn parse_one_u32(directive: &Directive) -> Option<u32> {
    parse_u32(&exactly_one_argument(directive)?.value)
}

fn parse_one_u64(directive: &Directive) -> Option<u64> {
    parse_u64(&exactly_one_argument(directive)?.value)
}

fn parse_u16(value: &[u8]) -> Option<u16> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

fn parse_retry_on(arguments: &[super::Word]) -> Result<RetryOn, String> {
    if arguments.is_empty() {
        return Err("requires at least one error form".into());
    }
    if arguments.len() == 1 && arguments[0].value == b"none" {
        return Ok(RetryOn::None);
    }
    if arguments.iter().any(|argument| argument.value == b"none") {
        return Err("none cannot be combined with another error form".into());
    }
    if arguments
        .iter()
        .any(|argument| matches!(argument.value.as_slice(), b"all" | b"all-retryable-errors"))
    {
        if arguments.len() != 1 {
            return Err(
                "all and all-retryable-errors cannot be combined with another error form".into(),
            );
        }
        return Ok(RetryOn::Rules {
            triggers: vec![
                RetryOnTrigger::ConnFailure,
                RetryOnTrigger::EmptyResponse,
                RetryOnTrigger::ResponseTimeout,
                RetryOnTrigger::JunkResponse,
            ],
            response_statuses: (500..=599).collect(),
        });
    }

    let mut seen = HashSet::new();
    let mut triggers = Vec::new();
    let mut response_statuses = Vec::new();
    for argument in arguments {
        if !seen.insert(argument.value.clone()) {
            return Err(format!(
                "duplicate error form `{}`",
                display_bytes(&argument.value)
            ));
        }
        let trigger = match argument.value.as_slice() {
            b"conn-failure" | b"conn-refused" => Some(RetryOnTrigger::ConnFailure),
            b"empty-response" => Some(RetryOnTrigger::EmptyResponse),
            b"response-timeout" => Some(RetryOnTrigger::ResponseTimeout),
            b"junk-response" => Some(RetryOnTrigger::JunkResponse),
            b"all" | b"all-retryable-errors" => unreachable!("all forms returned above"),
            b"0rtt-rejected" => {
                return Err("0rtt-rejected is outside the supported retry trigger subset".into());
            }
            _ => None,
        };
        if let Some(trigger) = trigger {
            triggers.push(trigger);
            continue;
        }

        let status = parse_u16(&argument.value)
            .filter(|_| argument.value.len() == 3 && argument.value.iter().all(u8::is_ascii_digit))
            .ok_or_else(|| {
                format!(
                    "unsupported error form `{}`",
                    display_bytes(&argument.value)
                )
            })?;
        if !(500..=599).contains(&status) {
            return Err(format!(
                "status `{}` is not a supported 5xx retry status",
                display_bytes(&argument.value)
            ));
        }
        if response_statuses.contains(&status) {
            return Err(format!(
                "duplicate error form `{}`",
                display_bytes(&argument.value)
            ));
        }
        response_statuses.push(status);
    }
    response_statuses.sort_unstable();
    Ok(RetryOn::Rules {
        triggers,
        response_statuses,
    })
}

fn parse_u32(value: &[u8]) -> Option<u32> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

fn parse_u64(value: &[u8]) -> Option<u64> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

fn parse_i32(value: &[u8]) -> Option<i32> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

fn parse_duration(value: &[u8]) -> Option<Duration> {
    let number_len = value
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if number_len == 0 {
        return None;
    }
    let amount = parse_u64(&value[..number_len])?;
    match &value[number_len..] {
        b"us" => Some(Duration::from_micros(amount)),
        b"" | b"ms" => Some(Duration::from_millis(amount)),
        b"s" => Some(Duration::from_secs(amount)),
        b"m" => Some(Duration::from_secs(amount.checked_mul(60)?)),
        b"h" => Some(Duration::from_secs(amount.checked_mul(60 * 60)?)),
        b"d" => Some(Duration::from_secs(amount.checked_mul(24 * 60 * 60)?)),
        _ => None,
    }
}

fn parse_bind_address(value: &[u8]) -> Option<BindAddress> {
    let unix_path = value.strip_prefix(b"unix@").unwrap_or(value);
    if unix_path.starts_with(b"/") {
        return Some(BindAddress::Unix {
            path: unix_path.to_vec(),
        });
    }
    let (host, port) = parse_host_port(value)?;
    Some(BindAddress::Tcp { host, port })
}

enum BindParseError {
    Malformed,
    Semantic(String),
    Conflict {
        name: Vec<u8>,
        current_span: Span,
        previous_span: Span,
    },
}

#[derive(Default)]
struct BindOptions<'a> {
    ssl: Option<Span>,
    certificate: Option<&'a super::Word>,
    alpn: Option<(Vec<TlsAlpn>, Span)>,
    minimum_version: Option<(TlsMinimumVersion, Span)>,
    maxconn: Option<(u64, Span)>,
    mode: Option<(u16, Span)>,
}

fn parse_bind(
    directive: &Directive,
    occurrence: OccurrenceId,
) -> Result<Vec<EffectiveBind>, BindParseError> {
    let (address_word, option_words) = directive
        .arguments
        .split_first()
        .ok_or(BindParseError::Malformed)?;
    let addresses = address_word
        .value
        .split(|byte| *byte == b',')
        .map(parse_bind_address)
        .collect::<Option<Vec<_>>>()
        .ok_or(BindParseError::Malformed)?;
    let options = parse_bind_options(option_words)?;
    let tls = finish_bind_tls(&options, occurrence)?;
    if options.mode.is_some()
        && addresses
            .iter()
            .any(|address| !matches!(address, BindAddress::Unix { .. }))
    {
        return Err(BindParseError::Semantic(
            "HAProxy bind mode applies only to Unix sockets".into(),
        ));
    }
    let mode = options
        .mode
        .map(|(value, span)| EffectiveValue::direct(value, occurrence, span));
    let maxconn = options
        .maxconn
        .map(|(value, span)| EffectiveValue::direct(value, occurrence, span));
    Ok(addresses
        .into_iter()
        .map(|address| EffectiveBind {
            address: EffectiveValue::direct(address, occurrence, address_word.span),
            mode: mode.clone(),
            maxconn: maxconn.clone(),
            tls: tls.clone(),
        })
        .collect())
}

fn parse_bind_options(options: &[super::Word]) -> Result<BindOptions<'_>, BindParseError> {
    let mut parsed = BindOptions::default();
    let mut index = 0;
    while index < options.len() {
        let option = &options[index];
        match option.value.as_slice() {
            b"ssl" if parsed.ssl.is_none() => {
                parsed.ssl = Some(option.span);
                index += 1;
            }
            b"crt" if parsed.certificate.is_none() => {
                parsed.certificate = Some(options.get(index + 1).ok_or_else(|| {
                    BindParseError::Semantic(
                        "HAProxy bind certificate selection is incomplete".into(),
                    )
                })?);
                index += 2;
            }
            b"alpn" if parsed.alpn.is_none() => {
                let protocols = options.get(index + 1).ok_or_else(|| {
                    BindParseError::Semantic("HAProxy bind ALPN policy is incomplete".into())
                })?;
                let alpn = parse_tls_alpn(&protocols.value).ok_or_else(|| {
                    BindParseError::Semantic(
                        "HAProxy bind ALPN policy is not exactly representable".into(),
                    )
                })?;
                parsed.alpn = Some((alpn, protocols.span));
                index += 2;
            }
            b"ssl-min-ver" if parsed.minimum_version.is_none() => {
                let version = options.get(index + 1).ok_or_else(|| {
                    BindParseError::Semantic(
                        "HAProxy bind TLS minimum version is incomplete".into(),
                    )
                })?;
                let minimum_version = match version.value.as_slice() {
                    b"TLSv1.2" => TlsMinimumVersion::Tls12,
                    b"TLSv1.3" => TlsMinimumVersion::Tls13,
                    _ => {
                        return Err(BindParseError::Semantic(
                            "HAProxy bind TLS minimum version is not represented canonically"
                                .into(),
                        ));
                    }
                };
                parsed.minimum_version = Some((minimum_version, version.span));
                index += 2;
            }
            b"maxconn" if parsed.maxconn.is_none() => {
                let value = options.get(index + 1).ok_or_else(|| {
                    BindParseError::Semantic(
                        "HAProxy bind maxconn requires an unsigned integer".into(),
                    )
                })?;
                let maxconn = parse_u64(&value.value).ok_or_else(|| {
                    BindParseError::Semantic(
                        "HAProxy bind maxconn requires an unsigned integer".into(),
                    )
                })?;
                parsed.maxconn = Some((maxconn, value.span));
                index += 2;
            }
            b"mode" if parsed.mode.is_none() => {
                parsed.mode = Some(parse_bind_mode(options.get(index + 1))?);
                index += 2;
            }
            b"crt" | b"crt-list" => {
                return Err(BindParseError::Semantic(
                    "HAProxy bind certificate selection uses crt-list or multiple crt parameters"
                        .into(),
                ));
            }
            b"ssl" | b"alpn" | b"ssl-min-ver" | b"maxconn" | b"mode" => {
                let previous_span = match option.value.as_slice() {
                    b"ssl" => parsed.ssl,
                    b"alpn" => parsed.alpn.as_ref().map(|(_, span)| *span),
                    b"ssl-min-ver" => parsed.minimum_version.map(|(_, span)| span),
                    b"maxconn" => parsed.maxconn.map(|(_, span)| span),
                    _ => parsed.mode.map(|(_, span)| span),
                }
                .expect("duplicate option has a first span");
                return Err(BindParseError::Conflict {
                    name: option.value.clone(),
                    current_span: option.span,
                    previous_span,
                });
            }
            _ => {
                return Err(BindParseError::Semantic(
                    "HAProxy bind option is not represented by the import IR".into(),
                ));
            }
        }
    }
    Ok(parsed)
}

fn parse_bind_mode(value: Option<&super::Word>) -> Result<(u16, Span), BindParseError> {
    let value = value.ok_or_else(|| {
        BindParseError::Semantic("HAProxy bind mode requires octal permission bits".into())
    })?;
    let mode = std::str::from_utf8(&value.value)
        .ok()
        .and_then(|value| u16::from_str_radix(value, 8).ok())
        .filter(|mode| (1..=0o777).contains(mode))
        .ok_or_else(|| {
            BindParseError::Semantic(
                "HAProxy bind mode requires octal permission bits from 001 through 777".into(),
            )
        })?;
    Ok((mode, value.span))
}

