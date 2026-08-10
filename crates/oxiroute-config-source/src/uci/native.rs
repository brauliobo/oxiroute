pub(crate) fn decode(source: &str) -> Result<Value, ConfigSourceError> {
    let document = parse_uci_document(source.as_bytes())?;
    decode_document(&document)
}

pub(crate) fn decode_with_directives(
    source: &str,
) -> Result<(Value, Vec<NativeDirective>), ConfigSourceError> {
    let document = parse_uci_document(source.as_bytes())?;
    let mut json_sections = Vec::new();
    let mut main = None;
    let mut directives = Vec::new();
    for section in document.sections {
        match section.section_type.as_str() {
            "json" => json_sections.push(section),
            "oxiroute" => {
                if section.name != "main" {
                    return Err(ConfigSourceError::parse(
                        "UCI",
                        "the oxiroute section must be named `main`",
                    ));
                }
                if main.is_some() {
                    return Err(ConfigSourceError::parse(
                        "UCI",
                        "only one oxiroute `main` section is allowed",
                    ));
                }
                main = Some(decode_main_section(&section)?);
            }
            "nginx_server" => {
                directives.push(NativeDirective::Nginx(decode_nginx_section(&section)?));
            }
            "haproxy_server" => {
                directives.push(NativeDirective::Haproxy(decode_haproxy_section(&section)?));
            }
            "squid_server" => {
                directives.push(NativeDirective::Squid(decode_squid_section(&section)?));
            }
            "apache_server" => {
                directives.push(NativeDirective::Apache(decode_apache_section(&section)?));
            }
            "varnish_server" => {
                directives.push(NativeDirective::Varnish(decode_varnish_section(&section)?));
            }
            section_type => {
                return Err(ConfigSourceError::parse(
                    "UCI",
                    format!("unknown section type `{section_type}`"),
                ));
            }
        }
    }

    let mut value = if json_sections.is_empty() {
        Value::Object(Map::new())
    } else {
        decode_document(&UciDocument {
            sections: json_sections,
        })?
    };
    if let Some(main) = main {
        let Value::Object(root) = &mut value else {
            return Err(ConfigSourceError::parse(
                "UCI",
                "generic JSON root must be an object when oxiroute `main` is present",
            ));
        };
        for (key, value) in main {
            if root.insert(key.clone(), value).is_some() {
                return Err(ConfigSourceError::parse(
                    "UCI",
                    format!("oxiroute `main` repeats generic root field `{key}`"),
                ));
            }
        }
    }
    Ok((value, directives))
}

fn decode_squid_section(
    section: &UciSection,
) -> Result<crate::native::SquidSource, ConfigSourceError> {
    let mut object = Map::new();
    for entry in &section.entries {
        match entry {
            UciEntry::Option { name, value } if name == "path" => {
                if object
                    .insert(name.clone(), Value::String(value.clone()))
                    .is_some()
                {
                    return Err(ConfigSourceError::parse(
                        "UCI",
                        format!("duplicate squid_server option `{name}`"),
                    ));
                }
            }
            UciEntry::Option { name, value } if name == "externalize_cache" => {
                if object
                    .insert(
                        name.clone(),
                        Value::Bool(parse_uci_bool(section, name, value)?),
                    )
                    .is_some()
                {
                    return Err(ConfigSourceError::parse(
                        "UCI",
                        format!("duplicate squid_server option `{name}`"),
                    ));
                }
            }
            UciEntry::Option { name, .. } | UciEntry::List { name, .. } => {
                return Err(ConfigSourceError::parse(
                    "UCI",
                    format!("unknown squid_server entry `{name}`"),
                ));
            }
        }
    }
    decode_squid(Value::Object(object), "UCI")
}

fn decode_apache_section(
    section: &UciSection,
) -> Result<crate::native::ApacheSource, ConfigSourceError> {
    let mut object = Map::new();
    for entry in &section.entries {
        let UciEntry::Option { name, value } = entry else {
            return Err(ConfigSourceError::parse(
                "UCI",
                format!(
                    "apache_server `{}` accepts only option entries",
                    section.name
                ),
            ));
        };
        if name != "path" {
            return Err(ConfigSourceError::parse(
                "UCI",
                format!("unknown apache_server option `{name}`"),
            ));
        }
        if object
            .insert(name.clone(), Value::String(value.clone()))
            .is_some()
        {
            return Err(ConfigSourceError::parse(
                "UCI",
                "duplicate apache_server option `path`",
            ));
        }
    }
    decode_apache(Value::Object(object), "UCI")
}

fn decode_varnish_section(
    section: &UciSection,
) -> Result<crate::native::VarnishSource, ConfigSourceError> {
    let mut object = Map::new();
    let mut arguments = Vec::new();
    for entry in &section.entries {
        match entry {
            UciEntry::Option { name, value } if name == "path" => {
                if object
                    .insert(name.clone(), Value::String(value.clone()))
                    .is_some()
                {
                    return Err(ConfigSourceError::parse(
                        "UCI",
                        "duplicate varnish_server option `path`",
                    ));
                }
            }
            UciEntry::List { name, value } if name == "arguments" => {
                arguments.push(Value::String(value.clone()));
            }
            UciEntry::Option { name, .. } | UciEntry::List { name, .. } => {
                return Err(ConfigSourceError::parse(
                    "UCI",
                    format!("unknown varnish_server entry `{name}`"),
                ));
            }
        }
    }
    object.insert("arguments".to_owned(), Value::Array(arguments));
    decode_varnish(Value::Object(object), "UCI")
}

fn decode_main_section(section: &UciSection) -> Result<Map<String, Value>, ConfigSourceError> {
    let mut root = Map::new();
    for entry in &section.entries {
        let UciEntry::Option { name, value } = entry else {
            return Err(ConfigSourceError::parse(
                "UCI",
                "oxiroute `main` accepts only scalar option entries",
            ));
        };
        let value = match name.as_str() {
            "version" => Value::Number(parse_uci_integer::<u32>(section, name, value)?.into()),
            "max_connections" => {
                Value::Number(parse_uci_integer::<u64>(section, name, value)?.into())
            }
            _ => {
                return Err(ConfigSourceError::parse(
                    "UCI",
                    format!("unknown oxiroute `main` option `{name}`"),
                ));
            }
        };
        root.insert(name.clone(), value);
    }
    Ok(root)
}

fn decode_nginx_section(
    section: &UciSection,
) -> Result<crate::native::NginxSource, ConfigSourceError> {
    let allowed = [
        "path",
        "root_prefix",
        "host_timezone",
        "default_access_log_file",
        "recording_root",
        "default_error_server",
        "x_accel_controls_absent",
    ];
    let mut object = Map::new();
    for entry in &section.entries {
        let UciEntry::Option { name, value } = entry else {
            return Err(ConfigSourceError::parse(
                "UCI",
                format!(
                    "nginx_server `{}` accepts only option entries",
                    section.name
                ),
            ));
        };
        if !allowed.contains(&name.as_str()) {
            return Err(ConfigSourceError::parse(
                "UCI",
                format!("unknown nginx_server option `{name}`"),
            ));
        }
        object.insert(
            name.clone(),
            if name == "x_accel_controls_absent" {
                Value::Bool(parse_uci_bool(section, name, value)?)
            } else {
                Value::String(value.clone())
            },
        );
    }
    decode_nginx(Value::Object(object), "UCI")
}

fn decode_haproxy_section(
    section: &UciSection,
) -> Result<crate::native::HaproxySource, ConfigSourceError> {
    let mut paths = Vec::new();
    let mut object = Map::new();
    for entry in &section.entries {
        match entry {
            UciEntry::List { name, value } if name == "path" => {
                paths.push(Value::String(value.clone()));
            }
            UciEntry::Option { name, value } if name == "node_ip" => {
                object.insert(name.clone(), Value::String(value.clone()));
            }
            UciEntry::Option { name, value } if name == "gpu1_defined" => {
                object.insert(
                    name.clone(),
                    Value::Bool(parse_uci_bool(section, name, value)?),
                );
            }
            UciEntry::Option { name, .. } | UciEntry::List { name, .. } => {
                return Err(ConfigSourceError::parse(
                    "UCI",
                    format!("unknown haproxy_server entry `{name}`"),
                ));
            }
        }
    }
    object.insert("paths".to_owned(), Value::Array(paths));
    decode_haproxy(Value::Object(object), "UCI")
}

fn parse_uci_integer<T>(
    section: &UciSection,
    name: &str,
    value: &str,
) -> Result<T, ConfigSourceError>
where
    T: std::str::FromStr,
{
    value.parse().map_err(|_| {
        ConfigSourceError::parse(
            "UCI",
            format!(
                "section `{}` option `{name}` must be an integer",
                section.name
            ),
        )
    })
}

fn parse_uci_bool(
    section: &UciSection,
    name: &str,
    value: &str,
) -> Result<bool, ConfigSourceError> {
    match value {
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        _ => Err(ConfigSourceError::parse(
            "UCI",
            format!(
                "section `{}` option `{name}` must be a boolean",
                section.name
            ),
        )),
    }
}

