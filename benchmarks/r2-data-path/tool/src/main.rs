//! `r2tool`: the in-tree upstream and deterministic load generator of the R2 data-path harness.
//!
//! `r2tool upstream --listen ADDR [--echo-listen ADDR]` serves fixed bodies, a streaming body, a request
//! sink and an upgrade/TCP echo. `r2tool load ...` drives a fixed number of requests and prints
//! `key=value` lines. Synchronisation with the outside world is by hooks (`--hook-start` and
//! `--hook-stop` run to completion before and after the measured phase), never by timers.

mod load;
mod upstream;

use std::{collections::HashMap, process::ExitCode};

pub type Flags = HashMap<String, String>;

fn parse_flags(args: impl Iterator<Item = String>) -> Flags {
    let mut flags = Flags::new();
    let mut args = args.peekable();
    while let Some(flag) = args.next() {
        let name = flag
            .strip_prefix("--")
            .unwrap_or_else(|| panic!("unexpected argument {flag}"));
        let value = match args.peek() {
            Some(next) if !next.starts_with("--") => args.next().expect("peeked"),
            _ => "true".to_owned(),
        };
        flags.insert(name.to_owned(), value);
    }
    flags
}

pub fn flag<T: std::str::FromStr>(flags: &Flags, name: &str, default: T) -> T {
    flags.get(name).map_or(default, |value| {
        value
            .parse()
            .unwrap_or_else(|_| panic!("invalid value for --{name}"))
    })
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_default();
    let flags = parse_flags(args);
    match command.as_str() {
        "upstream" => upstream::run(&flags),
        "load" => return load::run(&flags),
        _ => {
            eprintln!("usage: r2tool upstream|load [--flag value]...");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}
