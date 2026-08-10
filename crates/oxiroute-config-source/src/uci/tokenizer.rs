fn tokenize_line(line: &str, line_number: usize) -> Result<Vec<String>, ConfigSourceError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut started = false;
    while let Some(character) = chars.next() {
        match character {
            ' ' | '\t' if started => {
                tokens.push(std::mem::take(&mut current));
                started = false;
            }
            ' ' | '\t' => {}
            '#' => break,
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(character) => current.push(character),
                        None => return Err(uci_parse(line_number, "unterminated single quote")),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => {
                            let escaped = chars.next().ok_or_else(|| {
                                uci_parse(line_number, "unterminated double-quote escape")
                            })?;
                            current.push(match escaped {
                                'n' => '\n',
                                'r' => '\r',
                                't' => '\t',
                                '\\' => '\\',
                                '"' => '"',
                                '\'' => '\'',
                                _ => {
                                    return Err(uci_parse(
                                        line_number,
                                        format!("unsupported escape `\\{escaped}`"),
                                    ));
                                }
                            });
                        }
                        Some(character) => current.push(character),
                        None => return Err(uci_parse(line_number, "unterminated double quote")),
                    }
                }
            }
            '\\' => {
                started = true;
                current.push(
                    chars
                        .next()
                        .ok_or_else(|| uci_parse(line_number, "trailing unquoted escape"))?,
                );
            }
            character => {
                started = true;
                current.push(character);
            }
        }
    }
    if started {
        tokens.push(current);
    }
    Ok(tokens)
}

fn quote_token(value: &str) -> String {
    if value.contains(['\n', '\r', '\t']) || value.chars().any(char::is_control) {
        let mut quoted = String::from("\"");
        for character in value.chars() {
            match character {
                '\n' => quoted.push_str("\\n"),
                '\r' => quoted.push_str("\\r"),
                '\t' => quoted.push_str("\\t"),
                '\\' => quoted.push_str("\\\\"),
                '"' => quoted.push_str("\\\""),
                character => quoted.push(character),
            }
        }
        quoted.push('"');
        quoted
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn render_name(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        value.to_owned()
    } else {
        quote_token(value)
    }
}

fn uci_parse(line: usize, message: impl Into<String>) -> ConfigSourceError {
    ConfigSourceError::parse("UCI", format!("line {line}: {}", message.into()))
}
