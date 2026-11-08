use std::{
    collections::HashSet,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

/// Decides which filesystem events can change the resolved configuration.
///
/// The configuration directory is watched so that rename-based replacement is observed, but the
/// directory also holds editor swap files, logs and tooling output. Only events that name the
/// configuration file, its directory or one of its recorded native dependencies count; an event
/// that does not name a path (a rescan request) always counts. Clones share one dependency set.
#[derive(Clone, Debug)]
pub struct ConfigEventFilter {
    directory: PathBuf,
    file_name: OsString,
    dependencies: Arc<RwLock<HashSet<PathBuf>>>,
}

impl ConfigEventFilter {
    /// A filter for the canonical file at `config`, watched through its parent directory.
    #[must_use]
    pub fn new(config: &Path, dependencies: &[PathBuf]) -> Self {
        let filter = Self {
            directory: config
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
            file_name: config.file_name().map(OsString::from).unwrap_or_default(),
            dependencies: Arc::default(),
        };
        filter.set_dependencies(dependencies);
        filter
    }

    /// Replaces the recorded dependency set after a (re)resolution.
    pub fn set_dependencies(&self, dependencies: &[PathBuf]) {
        *self
            .dependencies
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            dependencies.iter().cloned().collect();
    }

    /// Whether `event` may have changed the configuration or a native input of it.
    #[must_use]
    pub fn matches(&self, event: &notify::Event) -> bool {
        event.need_rescan()
            || event.paths.is_empty()
            || event.paths.iter().any(|path| self.is_relevant(path))
    }

    fn is_relevant(&self, path: &Path) -> bool {
        let names_config =
            path.parent() == Some(&self.directory) && path.file_name() == Some(&self.file_name);
        names_config || path == self.directory || {
            let dependencies = self
                .dependencies
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            dependencies.contains(path)
                || path
                    .parent()
                    .is_some_and(|parent| dependencies.contains(parent))
        }
    }
}

#[cfg(test)]
mod tests {
    use notify::{Event, EventKind, event::ModifyKind};

    use super::*;

    fn event(path: &str) -> Event {
        Event::new(EventKind::Modify(ModifyKind::Any)).add_path(PathBuf::from(path))
    }

    #[test]
    fn only_the_config_file_its_directory_and_dependencies_count() {
        let filter = ConfigEventFilter::new(
            Path::new("/etc/example/oxiroute.kdl"),
            &[
                PathBuf::from("/etc/example/native/site.cfg"),
                PathBuf::from("/etc/example/sites"),
            ],
        );

        assert!(filter.matches(&event("/etc/example/oxiroute.kdl")));
        assert!(filter.matches(&event("/etc/example")));
        assert!(filter.matches(&event("/etc/example/native/site.cfg")));
        assert!(filter.matches(&event("/etc/example/sites/new.conf")));
        assert!(!filter.matches(&event("/etc/example/oxiroute.kdl.swp")));
        assert!(!filter.matches(&event("/etc/example/notes.txt")));
        assert!(!filter.matches(&event("/etc/example/native/other.cfg")));
    }

    #[test]
    fn events_without_a_path_and_rescans_always_count() {
        let filter = ConfigEventFilter::new(Path::new("oxiroute.kdl"), &[]);

        assert!(filter.matches(&Event::new(EventKind::Any)));
        assert!(filter.matches(&event("./oxiroute.kdl")));
        assert!(!filter.matches(&event("./other.kdl")));
    }

    #[test]
    fn dependency_updates_are_shared_by_clones() {
        let filter = ConfigEventFilter::new(Path::new("/etc/example/oxiroute.kdl"), &[]);
        let clone = filter.clone();
        assert!(!clone.matches(&event("/var/native.cfg")));

        filter.set_dependencies(&[PathBuf::from("/var/native.cfg")]);

        assert!(clone.matches(&event("/var/native.cfg")));
    }
}
