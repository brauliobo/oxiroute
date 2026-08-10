#[cfg(unix)]
fn push_nginx_dependencies(
    graph: &oxiroute_import::nginx::SourceGraph,
    dependencies: &mut Dependencies,
) -> Result<(), ConfigSourceError> {
    for source in &graph.sources {
        dependencies.push_with_parent(&source.canonical_path)?;
    }
    let include_base = graph
        .root
        .and_then(|root| graph.source(root))
        .and_then(|source| source.canonical_path.parent())
        .unwrap_or_else(|| Path::new("."));
    for edge in &graph.includes {
        dependencies.push_include_parent(include_base, &path_from_bytes(&edge.pattern))?;
    }
    Ok(())
}

#[cfg(unix)]
fn push_apache_dependencies(
    graph: &oxiroute_import::apache::SourceGraph,
    dependencies: &mut Dependencies,
) -> Result<(), ConfigSourceError> {
    for source in &graph.sources {
        dependencies.push_with_parent(&source.canonical_path)?;
    }
    let include_base = graph
        .root
        .and_then(|root| graph.source(root))
        .and_then(|source| source.canonical_path.parent())
        .unwrap_or_else(|| Path::new("."));
    for edge in &graph.includes {
        dependencies.push_include_parent(include_base, &path_from_bytes(&edge.pattern))?;
    }
    Ok(())
}

#[cfg(unix)]
fn push_varnish_dependencies(
    graph: &oxiroute_import::varnish::SourceGraph,
    dependencies: &mut Dependencies,
) -> Result<(), ConfigSourceError> {
    for source in &graph.sources {
        if let Some(path) = &source.canonical_path {
            dependencies.push_with_parent(path)?;
        }
    }
    let include_base = graph
        .root
        .and_then(|root| graph.source(root))
        .and_then(|source| source.canonical_path.as_deref())
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."));
    for edge in &graph.includes {
        dependencies.push_include_parent(include_base, &path_from_bytes(&edge.pattern))?;
    }
    Ok(())
}

#[cfg(unix)]
fn push_squid_dependencies(
    graph: &oxiroute_import::squid::SourceGraph,
    dependencies: &mut Dependencies,
) -> Result<(), ConfigSourceError> {
    for source in &graph.sources {
        dependencies.push_with_parent(&source.canonical_path)?;
    }
    for edge in &graph.includes {
        for target in &edge.targets {
            dependencies.push_with_parent(&target.requested_path)?;
        }
        if let Some(directive) = graph
            .expanded_directives
            .iter()
            .find(|directive| directive.occurrence == edge.occurrence)
        {
            let base = graph
                .source(edge.source)
                .and_then(|source| source.canonical_path.parent())
                .unwrap_or_else(|| Path::new("."));
            for argument in &directive.directive.arguments {
                dependencies.push_include_parent(base, &path_from_bytes(&argument.value))?;
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(unix)]
fn has_glob_meta(path: &Path) -> bool {
    let mut escaped = false;
    for byte in path.as_os_str().as_bytes() {
        if escaped {
            escaped = false;
        } else if *byte == b'\\' {
            escaped = true;
        } else if matches!(*byte, b'*' | b'?' | b'[') {
            return true;
        }
    }
    false
}

#[cfg(unix)]
fn include_watch_parent(base: &Path, requested: &Path) -> PathBuf {
    let path = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        base.join(requested)
    };
    let mut prefix = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => prefix.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => prefix.push(".."),
            Component::Normal(segment) if has_glob_meta(Path::new(segment)) => return prefix,
            Component::Normal(segment) => prefix.push(segment),
            Component::Prefix(_) => unreachable!("Unix paths have no prefix component"),
        }
    }
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or(prefix, Path::to_path_buf)
}

fn failed_native_import<'a>(
    importer: &'static str,
    codes: impl IntoIterator<Item = &'a str>,
) -> ConfigSourceError {
    let mut counts = BTreeMap::<String, usize>::new();
    for code in codes {
        *counts.entry(code.to_owned()).or_default() += 1;
    }
    if counts.is_empty() {
        counts.insert("E_NON_FINAL_CANDIDATE".to_owned(), 1);
    }
    ConfigSourceError::NativeImport {
        importer,
        diagnostics: NativeDiagnostics {
            counts: counts
                .into_iter()
                .map(|(code, count)| NativeDiagnosticCount { code, count })
                .collect(),
        },
    }
}

fn resolve_path(parent: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        parent.join(path)
    }
}

const fn format_name(format: ConfigFormat) -> &'static str {
    match format {
        ConfigFormat::Kdl => "KDL 2",
        ConfigFormat::Lua => "Lua",
        ConfigFormat::Uci => "UCI",
        ConfigFormat::Hocon => "HOCON",
    }
}

#[derive(Default)]
struct Dependencies {
    paths: Vec<PathBuf>,
    seen: HashSet<PathBuf>,
}

impl Dependencies {
    fn push(&mut self, path: PathBuf) -> Result<(), ConfigSourceError> {
        if self.seen.insert(path.clone()) {
            if self.paths.len() == MAX_DEPENDENCY_PATHS {
                return Err(ConfigSourceError::DependencyLimit);
            }
            self.paths.push(path);
        }
        Ok(())
    }

    fn push_with_parent(&mut self, path: &Path) -> Result<(), ConfigSourceError> {
        self.push(path.to_path_buf())?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            self.push(parent.to_path_buf())?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn push_include_parent(
        &mut self,
        base: &Path,
        requested: &Path,
    ) -> Result<(), ConfigSourceError> {
        self.push(include_watch_parent(base, requested))
    }
}
