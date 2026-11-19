use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

const UNIT_NAME: &str = "oxiroute.service";
const DIRECTORIES: [&str; 4] = [
    "/etc/systemd/system",
    "/run/systemd/system",
    "/usr/lib/systemd/system",
    "/lib/systemd/system",
];
const HOMES: [&str; 3] = ["/home", "/root", "/run/user"];
const STATE_ROOTS: [(&str, &str); 4] = [
    ("StateDirectory", "/var/lib"),
    ("CacheDirectory", "/var/cache"),
    ("LogsDirectory", "/var/log"),
    ("RuntimeDirectory", "/run"),
];
const BIND_CAPABILITY: &str = "CAP_NET_BIND_SERVICE";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Access {
    Writable,
    ReadOnly,
    Hidden,
}

#[derive(Debug)]
struct Rule {
    path: PathBuf,
    access: Access,
}

#[derive(Debug)]
pub(super) struct Unit {
    pub(super) path: PathBuf,
    pub(super) serves: Option<PathBuf>,
    pub(super) grants_privileged_ports: bool,
    rules: Vec<Rule>,
    required: Vec<PathBuf>,
    working_directory: PathBuf,
    environment: Vec<(String, String)>,
    environment_files: Vec<PathBuf>,
}

pub(super) fn find(explicit: Option<&Path>) -> Result<Option<Unit>, String> {
    let located = explicit.map(Path::to_owned).or_else(|| {
        DIRECTORIES
            .iter()
            .map(|directory| Path::new(directory).join(UNIT_NAME))
            .find(|path| path.is_file())
    });
    located
        .map(|path| load(path, explicit.is_none()))
        .transpose()
}

fn load(path: PathBuf, merge_directories: bool) -> Result<Unit, String> {
    let read = |path: &Path| {
        fs::read_to_string(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
    };
    let mut dropins = if merge_directories {
        DIRECTORIES
            .iter()
            .flat_map(|directory| dropins(&Path::new(directory).join(format!("{UNIT_NAME}.d"))))
            .collect()
    } else {
        let mut name = path.clone().into_os_string();
        name.push(".d");
        dropins(Path::new(&name))
    };
    dropins.sort_by_key(|dropin| dropin.file_name().map(ToOwned::to_owned));
    let mut texts = vec![read(&path)?];
    for dropin in dropins {
        texts.push(read(&dropin)?);
    }
    Ok(Unit::parse(path, &texts))
}

fn dropins(directory: &Path) -> Vec<PathBuf> {
    fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "conf")
        })
        .collect()
}

fn service_settings(text: &str) -> Vec<(String, String)> {
    let mut settings = Vec::new();
    let mut in_service = false;
    let mut joined = String::new();
    for raw in text.lines().map(str::trim) {
        if joined.is_empty() && (raw.starts_with('#') || raw.starts_with(';')) {
            continue;
        }
        if let Some(continued) = raw.strip_suffix('\\') {
            joined.push_str(continued);
            joined.push(' ');
            continue;
        }
        joined.push_str(raw);
        let line = std::mem::take(&mut joined);
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            in_service = section == "Service";
        } else if in_service && let Some((key, value)) = line.split_once('=') {
            settings.push((key.trim().to_owned(), value.trim().to_owned()));
        }
    }
    settings
}

struct Settings(Vec<(String, String)>);

impl Settings {
    fn last(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    fn list(&self, key: &str) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(name, _)| name == key)
            .fold(Vec::new(), |mut words, (_, value)| {
                if value.is_empty() {
                    words.clear();
                } else {
                    words.extend(value.split_whitespace());
                }
                words
            })
    }

    fn enabled(&self, key: &str) -> bool {
        self.last(key)
            .is_some_and(|value| matches!(value, "yes" | "true" | "1" | "on"))
    }
}

fn rule(path: impl AsRef<Path>, access: Access) -> Rule {
    Rule {
        path: path.as_ref().to_owned(),
        access,
    }
}

fn listed(paths: Vec<&str>, access: Access) -> impl Iterator<Item = Rule> + '_ {
    paths
        .into_iter()
        .map(move |path| rule(path.trim_start_matches(['-', '+']), access))
}

impl Unit {
    pub(super) fn parse(path: PathBuf, texts: &[String]) -> Self {
        let settings = Settings(
            texts
                .iter()
                .flat_map(|text| service_settings(text))
                .collect(),
        );
        let mut rules = match settings.last("ProtectSystem") {
            Some("strict") => vec![rule("/", Access::ReadOnly)],
            Some("full") => ["/usr", "/boot", "/efi", "/etc"]
                .map(|path| rule(path, Access::ReadOnly))
                .into(),
            _ if settings.enabled("ProtectSystem") => ["/usr", "/boot", "/efi"]
                .map(|path| rule(path, Access::ReadOnly))
                .into(),
            _ => Vec::new(),
        };
        match settings.last("ProtectHome") {
            Some("read-only") => rules.extend(HOMES.map(|path| rule(path, Access::ReadOnly))),
            Some("tmpfs") => rules.extend(HOMES.map(|path| rule(path, Access::Hidden))),
            _ if settings.enabled("ProtectHome") => {
                rules.extend(HOMES.map(|path| rule(path, Access::Hidden)));
            }
            _ => {}
        }
        if settings.enabled("PrivateTmp") {
            rules.extend(["/tmp", "/var/tmp"].map(|path| rule(path, Access::Writable)));
        }
        let mut created = Vec::new();
        for (key, root) in STATE_ROOTS {
            for name in settings.list(key) {
                let name = name.split(':').next().unwrap_or(name);
                created.push(Path::new(root).join(name));
            }
        }
        rules.extend(created.iter().map(|path| rule(path, Access::Writable)));
        let writable = settings.list("ReadWritePaths");
        let required = writable
            .iter()
            .filter(|path| !path.starts_with('-'))
            .map(|path| PathBuf::from(path.trim_start_matches('+')))
            .filter(|path| !created.contains(path))
            .collect();
        rules.extend(listed(writable, Access::Writable));
        rules.extend(listed(settings.list("ReadOnlyPaths"), Access::ReadOnly));
        rules.extend(listed(settings.list("InaccessiblePaths"), Access::Hidden));
        Self {
            serves: settings.last("ExecStart").and_then(serves),
            grants_privileged_ports: settings.last("User").is_none_or(|user| user == "root")
                || settings
                    .list("AmbientCapabilities")
                    .contains(&BIND_CAPABILITY),
            working_directory: settings.last("WorkingDirectory").map_or_else(
                || PathBuf::from("/"),
                |path| PathBuf::from(path.trim_start_matches('-')),
            ),
            environment: settings
                .list("Environment")
                .into_iter()
                .filter_map(|pair| assignment(pair.trim_matches('"')))
                .collect(),
            environment_files: settings
                .list("EnvironmentFile")
                .into_iter()
                .map(|file| PathBuf::from(file.trim_start_matches('-')))
                .collect(),
            path,
            rules,
            required,
        }
    }

    pub(super) fn environment(&self) -> Environment {
        let files = self
            .environment_files
            .iter()
            .filter_map(|file| fs::read_to_string(file).ok())
            .flat_map(|text| text.lines().filter_map(assignment).collect::<Vec<_>>());
        Environment(Some(
            files.chain(self.environment.iter().cloned()).collect(),
        ))
    }

    pub(super) fn absolute(&self, path: &Path) -> PathBuf {
        self.working_directory.join(path)
    }

    pub(super) fn access(&self, path: &Path) -> Access {
        self.rules
            .iter()
            .filter(|rule| path.starts_with(&rule.path))
            .max_by_key(|rule| rule.path.components().count())
            .map_or(Access::Writable, |rule| rule.access)
    }

    pub(super) fn missing_required(&self) -> impl Iterator<Item = &Path> {
        self.required
            .iter()
            .map(PathBuf::as_path)
            .filter(|path| !path.exists())
    }
}

fn serves(command: &str) -> Option<PathBuf> {
    let words = command.split_whitespace().collect::<Vec<_>>();
    let at = words.iter().position(|word| *word == "serve")?;
    words.get(at + 1).map(PathBuf::from)
}

fn assignment(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    let (name, value) = line
        .split_once('=')
        .filter(|_| !line.starts_with(['#', ';']))?;
    Some((
        name.trim().to_owned(),
        value.trim().trim_matches(['"', '\'']).to_owned(),
    ))
}

pub(super) struct Environment(Option<BTreeMap<String, String>>);

impl Environment {
    pub(super) const fn of_process() -> Self {
        Self(None)
    }

    pub(super) fn get(&self, name: &str) -> Option<OsString> {
        match &self.0 {
            Some(variables) => variables.get(name).map(OsString::from),
            None => std::env::var_os(name),
        }
    }
}
