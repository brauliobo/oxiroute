use std::{io, sync::OnceLock};

use oxiroute_supervisor_process::executable_build_id;

static OWN: OnceLock<String> = OnceLock::new();
static MASTER: OnceLock<String> = OnceLock::new();

/// Reads the build id of this process's executable once and returns it.
///
/// # Errors
///
/// Returns an error when the executable image cannot be read.
pub fn init_build_id() -> io::Result<&'static str> {
    if let Some(id) = OWN.get() {
        return Ok(id);
    }
    let id = executable_build_id()?;
    Ok(OWN.get_or_init(|| id))
}

/// The build id of this process once [`init_build_id`] ran.
#[must_use]
pub fn build_id() -> Option<&'static str> {
    OWN.get().map(String::as_str)
}

/// Records the build id the supervising master announced to this worker.
pub fn record_master_build_id(id: &str) {
    if !id.is_empty() {
        let _ = MASTER.set(id.to_owned());
    }
}

/// The build id of the supervising master, known to a supervised worker.
#[must_use]
pub fn master_build_id() -> Option<&'static str> {
    MASTER.get().map(String::as_str)
}
