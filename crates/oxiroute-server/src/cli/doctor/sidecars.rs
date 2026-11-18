use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

use super::diagnosis::{Check, Diagnosis};
use crate::{config::history::ConfigHistory, overlay::OverlayStore};

const GROUP_AND_OTHERS: u32 = 0o077;

pub(super) fn check(config: &Path) -> Vec<Diagnosis> {
    let canonical = fs::canonicalize(config).unwrap_or_else(|_| config.to_owned());
    let mut diagnoses = overlay(&OverlayStore::beside(&canonical));
    diagnoses.extend(history(&ConfigHistory::beside(&canonical)));
    diagnoses
}

fn open_to_others(path: &Path) -> Option<u32> {
    let mode = fs::metadata(path).ok()?.permissions().mode() & 0o7777;
    (mode & GROUP_AND_OTHERS != 0).then_some(mode)
}

fn overlay(store: &OverlayStore) -> Vec<Diagnosis> {
    let path = store.path();
    if !path.exists() {
        return Vec::new();
    }
    let subject = path.display().to_string();
    let mut diagnoses = Vec::new();
    if let Some(mode) = open_to_others(path) {
        diagnoses.push(
            Diagnosis::warning(
                Check::Files,
                &subject,
                format!("the overlay file is open to its group or other users (mode {mode:04o})"),
            )
            .fixed_by(format!("chmod 600 {subject}")),
        );
    }
    diagnoses.push(match store.load() {
        Ok(overlay) => Diagnosis::ok(
            Check::Files,
            subject,
            format!(
                "the overlay file holds {} persisted settings and is applied at every start and reload",
                overlay.settings()
            ),
        ),
        Err(error) => Diagnosis::warning(
            Check::Files,
            &subject,
            format!("{error}: the daemon starts without it and refuses --persist changes"),
        )
        .fixed_by(format!(
            "repair {subject}, or delete it with `oxiroute overlay clear`"
        )),
    });
    diagnoses
}

fn history(history: &ConfigHistory) -> Vec<Diagnosis> {
    let directory = history.directory();
    let Ok(listing) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let subject = directory.display().to_string();
    let open = std::iter::once(directory.to_owned())
        .chain(listing.filter_map(|item| Some(item.ok()?.path())))
        .filter_map(|path| open_to_others(&path).map(|mode| (path, mode)))
        .collect::<Vec<(PathBuf, u32)>>();
    if open.is_empty() {
        return vec![Diagnosis::ok(
            Check::Files,
            subject,
            "the configuration history is private to the daemon's user",
        )];
    }
    open.into_iter()
        .map(|(path, mode)| {
            let path = path.display().to_string();
            Diagnosis::warning(
                Check::Files,
                &path,
                format!("the configuration history is open to its group or other users (mode {mode:04o})"),
            )
            .fixed_by(format!("chmod -R go-rwx {subject}"))
        })
        .collect()
}
