use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use serde_json::{Map, Value};

use crate::ConfigSourceError;
use crate::limits::{
    MAX_STRUCTURAL_DEPTH, check_output, check_string, source_text, validate_value,
};
use crate::native::{NativeDirective, decode_haproxy, decode_nginx};

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

    for (line_index, raw_line) in source.split('\n').enumerate() {
        let line_number = line_index + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let tokens = tokenize_line(line, line_number)?;
        if tokens.is_empty() {
            continue;
        }
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
    let mut output = String::new();
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
            output.push('\n');
        }
        writeln!(
            output,
            "config {} {}",
            render_name(&section.section_type),
            quote_token(&section.name)
        )
        .expect("writing to String cannot fail");
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
            .expect("writing to String cannot fail");
        }
    }
    check_output(&output)?;
    Ok(output)
}

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
        object.insert(name.clone(), Value::String(value.clone()));
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

pub(crate) fn render(value: &Value) -> Result<String, ConfigSourceError> {
    let mut records = Vec::new();
    flatten_value(value, "root", None, &mut records);
    let sections = records
        .into_iter()
        .map(RenderRecord::into_section)
        .collect();
    render_uci_document(&UciDocument { sections })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordKind {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
}

impl RecordKind {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "object" => Some(Self::Object),
            "array" => Some(Self::Array),
            "string" => Some(Self::String),
            "number" => Some(Self::Number),
            "bool" => Some(Self::Bool),
            "null" => Some(Self::Null),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Object => "object",
            Self::Array => "array",
            Self::String => "string",
            Self::Number => "number",
            Self::Bool => "bool",
            Self::Null => "null",
        }
    }

    fn is_container(self) -> bool {
        matches!(self, Self::Object | Self::Array)
    }
}

struct Record<'a> {
    section: &'a UciSection,
    kind: RecordKind,
    parent: Option<&'a str>,
    key: Option<&'a str>,
    index: Option<usize>,
    value: Option<&'a str>,
}

fn decode_document(document: &UciDocument) -> Result<Value, ConfigSourceError> {
    let mut records = HashMap::<&str, Record<'_>>::new();
    for section in &document.sections {
        if section.section_type != "json" {
            return Err(ConfigSourceError::parse(
                "UCI",
                format!(
                    "generic document section `{}` must have type `json`",
                    section.name
                ),
            ));
        }
        let record = parse_record(section)?;
        records.insert(section.name.as_str(), record);
    }
    let Some(root) = records.get("root") else {
        return Err(ConfigSourceError::parse("UCI", "missing `root` section"));
    };
    if root.parent.is_some() || root.key.is_some() || root.index.is_some() {
        return Err(ConfigSourceError::parse(
            "UCI",
            "root record cannot have parent, key, or index",
        ));
    }
    detect_parent_cycles(&records)?;

    let mut children = HashMap::<&str, Vec<&str>>::new();
    for (name, record) in &records {
        if *name == "root" {
            continue;
        }
        let parent = record.parent.ok_or_else(|| {
            ConfigSourceError::parse("UCI", format!("record `{name}` is missing parent"))
        })?;
        if !records.contains_key(parent) {
            return Err(ConfigSourceError::parse(
                "UCI",
                format!("record `{name}` has unknown parent `{parent}`"),
            ));
        }
        children.entry(parent).or_default().push(name);
    }

    let mut visited = HashSet::new();
    let value = build_record("root", &records, &children, &mut visited, 0)?;
    if visited.len() != records.len() {
        let mut orphaned = records
            .keys()
            .filter(|name| !visited.contains(**name))
            .copied()
            .collect::<Vec<_>>();
        orphaned.sort_unstable();
        return Err(ConfigSourceError::parse(
            "UCI",
            format!("orphaned records: {}", orphaned.join(", ")),
        ));
    }
    validate_value(&value)?;
    Ok(value)
}

fn parse_record(section: &UciSection) -> Result<Record<'_>, ConfigSourceError> {
    if section
        .entries
        .iter()
        .any(|entry| matches!(entry, UciEntry::List { .. }))
    {
        return Err(ConfigSourceError::parse(
            "UCI",
            format!(
                "generic json record `{}` cannot contain list entries",
                section.name
            ),
        ));
    }
    let allowed = ["parent", "key", "index", "kind", "value"];
    for entry in &section.entries {
        let UciEntry::Option { name, .. } = entry else {
            unreachable!("lists were rejected above")
        };
        if !allowed.contains(&name.as_str()) {
            return Err(ConfigSourceError::parse(
                "UCI",
                format!("unknown option `{name}` in record `{}`", section.name),
            ));
        }
    }
    let kind_value = section.option("kind").ok_or_else(|| {
        ConfigSourceError::parse("UCI", format!("record `{}` is missing kind", section.name))
    })?;
    let kind = RecordKind::parse(kind_value).ok_or_else(|| {
        ConfigSourceError::parse(
            "UCI",
            format!("record `{}` has unknown kind `{kind_value}`", section.name),
        )
    })?;
    let key = section.option("key");
    let index = section
        .option("index")
        .map(|index| {
            index.parse::<usize>().map_err(|_| {
                ConfigSourceError::parse(
                    "UCI",
                    format!("record `{}` has invalid index `{index}`", section.name),
                )
            })
        })
        .transpose()?;
    if key.is_some() && index.is_some() {
        return Err(ConfigSourceError::parse(
            "UCI",
            format!("record `{}` cannot have both key and index", section.name),
        ));
    }
    let value = section.option("value");
    if kind.is_container() || kind == RecordKind::Null {
        if value.is_some() {
            return Err(ConfigSourceError::parse(
                "UCI",
                format!(
                    "record `{}` kind {} cannot have value",
                    section.name,
                    kind.as_str()
                ),
            ));
        }
    } else if value.is_none() {
        return Err(ConfigSourceError::parse(
            "UCI",
            format!(
                "record `{}` kind {} requires value",
                section.name,
                kind.as_str()
            ),
        ));
    }
    Ok(Record {
        section,
        kind,
        parent: section.option("parent"),
        key,
        index,
        value,
    })
}

fn detect_parent_cycles(records: &HashMap<&str, Record<'_>>) -> Result<(), ConfigSourceError> {
    let mut complete = HashSet::new();
    for name in records.keys().copied() {
        let mut path = Vec::new();
        let mut positions = HashMap::new();
        let mut current = name;
        while !complete.contains(current) {
            if let Some(position) = positions.insert(current, path.len()) {
                let mut cycle = path[position..].to_vec();
                cycle.push(current);
                return Err(ConfigSourceError::parse(
                    "UCI",
                    format!("parent cycle: {}", cycle.join(" -> ")),
                ));
            }
            path.push(current);
            let Some(parent) = records.get(current).and_then(|record| record.parent) else {
                break;
            };
            if !records.contains_key(parent) {
                break;
            }
            current = parent;
        }
        complete.extend(path);
    }
    Ok(())
}

fn build_record(
    name: &str,
    records: &HashMap<&str, Record<'_>>,
    children: &HashMap<&str, Vec<&str>>,
    visited: &mut HashSet<String>,
    depth: usize,
) -> Result<Value, ConfigSourceError> {
    if depth > MAX_STRUCTURAL_DEPTH {
        return Err(ConfigSourceError::StructuralDepth);
    }
    visited.insert(name.to_owned());
    let record = records.get(name).expect("record names are validated");
    let child_names = children.get(name).map_or(&[][..], Vec::as_slice);
    match record.kind {
        RecordKind::Object => {
            let mut object = Map::new();
            for child_name in child_names {
                let child = records.get(child_name).expect("child names are validated");
                let key = child.key.ok_or_else(|| {
                    ConfigSourceError::parse(
                        "UCI",
                        format!("object child `{child_name}` is missing key"),
                    )
                })?;
                if child.index.is_some() {
                    return Err(ConfigSourceError::parse(
                        "UCI",
                        format!("object child `{child_name}` cannot have index"),
                    ));
                }
                if object.contains_key(key) {
                    return Err(ConfigSourceError::parse(
                        "UCI",
                        format!("duplicate key `{key}` under record `{name}`"),
                    ));
                }
                object.insert(
                    key.to_owned(),
                    build_record(child_name, records, children, visited, depth + 1)?,
                );
            }
            Ok(Value::Object(object))
        }
        RecordKind::Array => {
            let mut indexed = Vec::with_capacity(child_names.len());
            let mut seen = HashSet::new();
            for child_name in child_names {
                let child = records.get(child_name).expect("child names are validated");
                if child.key.is_some() {
                    return Err(ConfigSourceError::parse(
                        "UCI",
                        format!("array child `{child_name}` cannot have key"),
                    ));
                }
                let index = child.index.ok_or_else(|| {
                    ConfigSourceError::parse(
                        "UCI",
                        format!("array child `{child_name}` is missing index"),
                    )
                })?;
                if !seen.insert(index) {
                    return Err(ConfigSourceError::parse(
                        "UCI",
                        format!("duplicate index `{index}` under record `{name}`"),
                    ));
                }
                indexed.push((index, *child_name));
            }
            indexed.sort_unstable_by_key(|(index, _)| *index);
            for (expected, (actual, _)) in indexed.iter().enumerate() {
                if expected != *actual {
                    return Err(ConfigSourceError::parse(
                        "UCI",
                        format!("gapped array under record `{name}`: expected index {expected}"),
                    ));
                }
            }
            indexed
                .into_iter()
                .map(|(_, child)| build_record(child, records, children, visited, depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
        }
        kind => {
            if !child_names.is_empty() {
                return Err(ConfigSourceError::parse(
                    "UCI",
                    format!("scalar record `{name}` cannot have children"),
                ));
            }
            scalar_value(record, kind)
        }
    }
}

fn scalar_value(record: &Record<'_>, kind: RecordKind) -> Result<Value, ConfigSourceError> {
    let value = record.value.unwrap_or_default();
    match kind {
        RecordKind::String => Ok(Value::String(value.to_owned())),
        RecordKind::Bool => match value {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(ConfigSourceError::parse(
                "UCI",
                format!(
                    "record `{}` has invalid bool `{value}`",
                    record.section.name
                ),
            )),
        },
        RecordKind::Number => {
            let parsed = serde_json::from_str::<Value>(value).map_err(|_| {
                ConfigSourceError::parse(
                    "UCI",
                    format!(
                        "record `{}` has invalid number `{value}`",
                        record.section.name
                    ),
                )
            })?;
            if parsed.is_number() {
                Ok(parsed)
            } else {
                Err(ConfigSourceError::parse(
                    "UCI",
                    format!(
                        "record `{}` has invalid number `{value}`",
                        record.section.name
                    ),
                ))
            }
        }
        RecordKind::Null => Ok(Value::Null),
        RecordKind::Object | RecordKind::Array => unreachable!("containers are built separately"),
    }
}

struct RenderRecord {
    name: String,
    parent: Option<String>,
    relation: Option<Relation>,
    kind: RecordKind,
    value: Option<String>,
}

enum Relation {
    Key(String),
    Index(usize),
}

impl RenderRecord {
    fn into_section(self) -> UciSection {
        let mut entries = Vec::new();
        if let Some(parent) = self.parent {
            entries.push(UciEntry::Option {
                name: "parent".to_owned(),
                value: parent,
            });
        }
        match self.relation {
            Some(Relation::Key(key)) => entries.push(UciEntry::Option {
                name: "key".to_owned(),
                value: key,
            }),
            Some(Relation::Index(index)) => entries.push(UciEntry::Option {
                name: "index".to_owned(),
                value: index.to_string(),
            }),
            None => {}
        }
        entries.push(UciEntry::Option {
            name: "kind".to_owned(),
            value: self.kind.as_str().to_owned(),
        });
        if let Some(value) = self.value {
            entries.push(UciEntry::Option {
                name: "value".to_owned(),
                value,
            });
        }
        UciSection {
            section_type: "json".to_owned(),
            name: self.name,
            entries,
        }
    }
}

fn flatten_value(
    value: &Value,
    name: &str,
    link: Option<(String, Relation)>,
    records: &mut Vec<RenderRecord>,
) {
    let (parent, relation) = link.map_or((None, None), |(parent, relation)| {
        (Some(parent), Some(relation))
    });
    let (kind, scalar) = match value {
        Value::Object(_) => (RecordKind::Object, None),
        Value::Array(_) => (RecordKind::Array, None),
        Value::String(value) => (RecordKind::String, Some(value.clone())),
        Value::Number(value) => (RecordKind::Number, Some(value.to_string())),
        Value::Bool(value) => (RecordKind::Bool, Some(value.to_string())),
        Value::Null => (RecordKind::Null, None),
    };
    records.push(RenderRecord {
        name: name.to_owned(),
        parent,
        relation,
        kind,
        value: scalar,
    });

    match value {
        Value::Object(object) => {
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            for (key, value) in entries {
                let child_name = format!("node-{:06}", records.len());
                flatten_value(
                    value,
                    &child_name,
                    Some((name.to_owned(), Relation::Key(key.clone()))),
                    records,
                );
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                let child_name = format!("node-{:06}", records.len());
                flatten_value(
                    value,
                    &child_name,
                    Some((name.to_owned(), Relation::Index(index))),
                    records,
                );
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

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
