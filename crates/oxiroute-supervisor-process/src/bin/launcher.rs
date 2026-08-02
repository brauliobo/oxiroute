use std::process::ExitCode;

#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    oxiroute_supervisor_process::launcher::run()
}

#[cfg(not(target_os = "linux"))]
fn main() -> ExitCode {
    eprintln!("oxiroute worker launcher requires Linux");
    ExitCode::FAILURE
}
