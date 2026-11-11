use std::process::{Command, Output};

pub fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_oxiroute"))
}

pub fn output_text(output: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
