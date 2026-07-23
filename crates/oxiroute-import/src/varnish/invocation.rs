#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VarnishdInvocation {
    pub arguments: Vec<String>,
}

impl VarnishdInvocation {
    #[must_use]
    pub fn new(arguments: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            arguments: arguments.into_iter().map(Into::into).collect(),
        }
    }

    #[must_use]
    pub fn facts(&self) -> InvocationFacts {
        InvocationFacts::from_arguments(&self.arguments)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InvocationFacts {
    pub storage: Vec<StorageFact>,
    pub startup: Vec<StartupFact>,
    pub unsupported_arguments: Vec<String>,
    pub truncated: bool,
}

impl InvocationFacts {
    fn from_arguments(arguments: &[String]) -> Self {
        let mut facts = Self::default();
        let mut index =
            usize::from(arguments.first().is_some_and(|argument| {
                !argument.starts_with('-') || argument.ends_with("varnishd")
            }));
        let mut processed = index;
        let mut processed_bytes = arguments.iter().take(index).map(String::len).sum::<usize>();
        while let Some(argument) = arguments.get(index) {
            let (flag, inline) = split_flag(argument);
            let value = inline.or_else(|| {
                arguments
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                    .map(String::as_str)
            });
            let consumed_value = inline.is_none() && value.is_some();
            let argument_count = 1 + usize::from(consumed_value);
            let argument_bytes = argument.len()
                + usize::from(consumed_value) * arguments.get(index + 1).map_or(0, String::len);
            if processed.saturating_add(argument_count) > MAX_INVOCATION_ARGUMENTS
                || processed_bytes.saturating_add(argument_bytes) > MAX_INVOCATION_BYTES
            {
                facts.truncated = true;
                break;
            }
            match (flag, value) {
                ("-s", Some(value)) => facts.storage.push(StorageFact::parse(value)),
                ("-a", Some(value)) => facts.startup.push(StartupFact::Listen(value.to_owned())),
                ("-T", Some(value)) => {
                    facts
                        .startup
                        .push(StartupFact::Management(value.to_owned()));
                }
                ("-f", Some(value)) => facts.startup.push(StartupFact::Vcl(value.to_owned())),
                ("-n", Some(value)) => facts.startup.push(StartupFact::Instance(value.to_owned())),
                ("-p", Some(value)) => facts
                    .startup
                    .push(StartupFact::Parameter(split_setting(value))),
                ("-j", Some(value)) => facts.startup.push(StartupFact::Jail(value.to_owned())),
                ("-S", Some(value)) => facts.startup.push(StartupFact::Secret(value.to_owned())),
                ("-P", Some(value)) => facts.startup.push(StartupFact::PidFile(value.to_owned())),
                ("-F", _) => facts.startup.push(StartupFact::Foreground),
                _ => facts.unsupported_arguments.push(argument.clone()),
            }
            index += argument_count;
            processed += argument_count;
            processed_bytes += argument_bytes;
        }
        facts
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageFact {
    pub name: Option<String>,
    pub kind: StorageKind,
    pub arguments: Vec<String>,
    pub raw: String,
}

impl StorageFact {
    fn parse(raw: &str) -> Self {
        let (name, specification) = raw
            .split_once('=')
            .map_or((None, raw), |(name, value)| (Some(name.to_owned()), value));
        let mut fields = specification.split(',');
        let kind_name = fields.next().unwrap_or_default();
        let kind = match kind_name {
            "malloc" => StorageKind::Malloc,
            "file" => StorageKind::File,
            "persistent" => StorageKind::Persistent,
            "deprecated_persistent" => StorageKind::DeprecatedPersistent,
            "umem" => StorageKind::Umem,
            "none" => StorageKind::None,
            other => StorageKind::Unknown(other.to_owned()),
        };
        Self {
            name,
            kind,
            arguments: fields.map(str::to_owned).collect(),
            raw: raw.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageKind {
    Malloc,
    File,
    Persistent,
    DeprecatedPersistent,
    Umem,
    None,
    Unknown(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartupFact {
    Listen(String),
    Management(String),
    Vcl(String),
    Instance(String),
    Parameter(Setting),
    Jail(String),
    Secret(String),
    PidFile(String),
    Foreground,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Setting {
    pub name: String,
    pub value: Option<String>,
}

fn split_flag(argument: &str) -> (&str, Option<&str>) {
    const VALUE_FLAGS: [&str; 9] = ["-s", "-a", "-T", "-f", "-n", "-p", "-j", "-S", "-P"];
    VALUE_FLAGS
        .into_iter()
        .find_map(|flag| argument.strip_prefix(flag).map(|value| (flag, value)))
        .map_or((argument, None), |(flag, value)| {
            (flag, (!value.is_empty()).then_some(value))
        })
}

fn split_setting(value: &str) -> Setting {
    value.split_once('=').map_or_else(
        || Setting {
            name: value.to_owned(),
            value: None,
        },
        |(name, value)| Setting {
            name: name.to_owned(),
            value: Some(value.to_owned()),
        },
    )
}
pub const MAX_INVOCATION_ARGUMENTS: usize = 4_096;
pub const MAX_INVOCATION_BYTES: usize = 1024 * 1024;
