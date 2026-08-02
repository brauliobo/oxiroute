use std::{env, ffi::OsStr, os::unix::ffi::OsStrExt as _, process::ExitCode};

mod worker;

pub(crate) const MARKER: &str = "--__oxiroute-worker-7f3c9d1e";
const RESERVED_PREFIX: &[u8] = b"--__oxiroute-worker-";

pub(crate) fn dispatch() -> Option<ExitCode> {
    let mut arguments = env::args_os();
    let _program = arguments.next();
    let marker = arguments.next()?;
    if marker != OsStr::new(MARKER) {
        return marker.as_bytes().starts_with(RESERVED_PREFIX).then(|| {
            eprintln!("OxiRoute supervised process failed: invalid reserved worker marker");
            ExitCode::FAILURE
        });
    }

    let result = worker::run(arguments);
    Some(match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("OxiRoute supervised process failed: {error}");
            ExitCode::FAILURE
        }
    })
}
