/// A parsed, ordered UCI document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UciDocument {
    /// Named sections in source order.
    pub sections: Vec<UciSection>,
}
/// A named UCI section with ordered options and lists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UciSection {
    /// The token following `config`.
    pub section_type: String,
    /// The required explicit section name.
    pub name: String,
    /// Section entries in source order.
    pub entries: Vec<UciEntry>,
}

impl UciSection {
    /// Returns the value of a uniquely parsed `option` entry.
    #[must_use]
    pub fn option(&self, name: &str) -> Option<&str> {
        self.entries.iter().find_map(|entry| match entry {
            UciEntry::Option {
                name: entry_name,
                value,
            } if entry_name == name => Some(value.as_str()),
            UciEntry::Option { .. } | UciEntry::List { .. } => None,
        })
    }
}

/// An option or list entry in a UCI section.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UciEntry {
    /// A singular `option name value` entry.
    Option { name: String, value: String },
    /// A `list name value` entry. Repeated values remain separate ordered entries.
    List { name: String, value: String },
}

/// Parses strict data-only UCI syntax without interpreting generic JSON records.
///
/// Anonymous sections, duplicate sections/options, option/list name conflicts, unknown commands,
/// and malformed quoting are rejected. Repeated `list` entries are preserved.
///
/// # Errors
///
/// Returns an error when the source exceeds a bound or violates the strict data-only UCI grammar.
pub fn parse_uci_document(source: &[u8]) -> Result<UciDocument, ConfigSourceError> {
    let source = source_text(source)?;
    let mut sections = Vec::<UciSection>::new();
    let mut section_names = HashSet::new();
    let mut option_names = HashSet::<String>::new();
    let mut list_names = HashSet::<String>::new();
    let mut parsed_nodes = 0;

    for (line_index, raw_line) in source.split('\n').enumerate() {
        let line_number = line_index + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let tokens = tokenize_line(line, line_number)?;
        if tokens.is_empty() {
            continue;
        }
        count_parsed_node(&mut parsed_nodes)?;
        match tokens[0].as_str() {
            "config" => {
                if tokens.len() != 3 {
                    return Err(uci_parse(line_number, "config requires a type and a name"));
                }
                check_string(&tokens[1])?;
                check_string(&tokens[2])?;
                if tokens[1].is_empty() || tokens[2].is_empty() {
                    return Err(uci_parse(
                        line_number,
                        "section type and name must not be empty",
                    ));
                }
                if !section_names.insert(tokens[2].clone()) {
                    return Err(uci_parse(
                        line_number,
                        format!("duplicate section `{}`", tokens[2]),
                    ));
                }
                sections.push(UciSection {
                    section_type: tokens[1].clone(),
                    name: tokens[2].clone(),
                    entries: Vec::new(),
                });
                option_names.clear();
                list_names.clear();
            }
            "option" | "list" => {
                if tokens.len() != 3 {
                    return Err(uci_parse(
                        line_number,
                        format!("{} requires a name and a value", tokens[0]),
                    ));
                }
                let Some(section) = sections.last_mut() else {
                    return Err(uci_parse(
                        line_number,
                        "entry appears before a config section",
                    ));
                };
                check_string(&tokens[1])?;
                check_string(&tokens[2])?;
                if tokens[1].is_empty() {
                    return Err(uci_parse(line_number, "entry name must not be empty"));
                }
                if tokens[0] == "option" {
                    if option_names.contains(&tokens[1]) || list_names.contains(&tokens[1]) {
                        return Err(uci_parse(
                            line_number,
                            format!("duplicate option `{}`", tokens[1]),
                        ));
                    }
                    option_names.insert(tokens[1].clone());
                    section.entries.push(UciEntry::Option {
                        name: tokens[1].clone(),
                        value: tokens[2].clone(),
                    });
                } else {
                    if option_names.contains(&tokens[1]) {
                        return Err(uci_parse(
                            line_number,
                            format!("list `{}` conflicts with an option", tokens[1]),
                        ));
                    }
                    list_names.insert(tokens[1].clone());
                    section.entries.push(UciEntry::List {
                        name: tokens[1].clone(),
                        value: tokens[2].clone(),
                    });
                }
            }
            command => {
                return Err(uci_parse(
                    line_number,
                    format!("unknown command `{command}`"),
                ));
            }
        }
    }
    Ok(UciDocument { sections })
}

/// Renders a UCI AST deterministically while preserving section and entry order.
///
/// # Errors
///
/// Returns an error for anonymous or duplicate sections, duplicate options, option/list conflicts,
/// strings that exceed their bound, or output that exceeds its bound.
pub fn render_uci_document(document: &UciDocument) -> Result<String, ConfigSourceError> {
    let mut output = BoundedOutput::new();
    let mut names = HashSet::new();
    for (index, section) in document.sections.iter().enumerate() {
        check_string(&section.section_type)?;
        check_string(&section.name)?;
        if section.section_type.is_empty() || section.name.is_empty() {
            return Err(ConfigSourceError::render(
                "UCI",
                "section type and name must not be empty",
            ));
        }
        if !names.insert(section.name.as_str()) {
            return Err(ConfigSourceError::render(
                "UCI",
                format!("duplicate section `{}`", section.name),
            ));
        }
        if index != 0 {
            output
                .write_char('\n')
                .map_err(|_| ConfigSourceError::OutputTooLarge)?;
        }
        writeln!(
            output,
            "config {} {}",
            render_name(&section.section_type),
            quote_token(&section.name)
        )
        .map_err(|_| ConfigSourceError::OutputTooLarge)?;
        let mut options = HashSet::new();
        let mut lists = HashSet::new();
        for entry in &section.entries {
            let (command, name, value) = match entry {
                UciEntry::Option { name, value } => {
                    if !options.insert(name.as_str()) || lists.contains(name.as_str()) {
                        return Err(ConfigSourceError::render(
                            "UCI",
                            format!("duplicate option `{name}`"),
                        ));
                    }
                    ("option", name, value)
                }
                UciEntry::List { name, value } => {
                    if options.contains(name.as_str()) {
                        return Err(ConfigSourceError::render(
                            "UCI",
                            format!("list `{name}` conflicts with an option"),
                        ));
                    }
                    lists.insert(name.as_str());
                    ("list", name, value)
                }
            };
            check_string(name)?;
            check_string(value)?;
            if name.is_empty() {
                return Err(ConfigSourceError::render(
                    "UCI",
                    "entry name must not be empty",
                ));
            }
            writeln!(
                output,
                "\t{command} {} {}",
                render_name(name),
                quote_token(value)
            )
            .map_err(|_| ConfigSourceError::OutputTooLarge)?;
        }
    }
    Ok(output.finish())
}

fn count_parsed_node(count: &mut usize) -> Result<(), ConfigSourceError> {
    *count = count.checked_add(1).ok_or(ConfigSourceError::NodeLimit)?;
    if *count > MAX_NODES {
        return Err(ConfigSourceError::NodeLimit);
    }
    Ok(())
}
