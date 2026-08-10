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
            entries.sort_unstable_by_key(|(left, _)| *left);
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

