use std::{
    env,
    ffi::OsString,
    fs::File,
    os::{
        fd::AsFd,
        unix::ffi::{OsStrExt, OsStringExt},
    },
    path::Path,
    process::{Command, ExitCode, Stdio},
};

use oxiroute_supervisor_process::{
    MAX_WORKER_ARGUMENTS, MAX_WORKER_ENVIRONMENT, MAX_WORKER_METADATA_BYTES,
    MAX_WORKER_METADATA_ITEM_BYTES,
};
use rustix::process::{Signal, getpgrp, kill_process_group};

type WorkerEnvironment = Vec<(OsString, OsString)>;
type LauncherResult<T> = Result<T, Box<dyn std::error::Error>>;
const WORKER_METADATA_VERSION: &str = "v2";

fn main() -> ExitCode {
    match launch() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("master fixture launcher: {error}");
            ExitCode::FAILURE
        }
    }
}

fn launch() -> LauncherResult<()> {
    let mut encoded = env::args_os().skip(1);
    let worker = encoded.next().ok_or("missing resolved worker executable")?;
    if !Path::new(&worker).is_absolute() {
        return Err("worker executable is not absolute".into());
    }
    let (cgroup_path, arguments, environment) = decode_metadata(&mut encoded)?;
    if encoded.next().is_some() {
        return Err("trailing launcher metadata".into());
    }
    if let Some(path) = cgroup_path.as_deref() {
        std::fs::write(
            Path::new(path).join("cgroup.procs"),
            std::process::id().to_string(),
        )?;
    }
    let mut child = Command::new(worker)
        .args(arguments)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::inherit())
        .spawn()?;
    let null = File::open("/dev/null")?;
    rustix::stdio::dup2_stdin(null.as_fd())?;
    let _worker_status = child.wait()?;
    if let Some(path) = cgroup_path.as_deref() {
        let _ = std::fs::write(Path::new(path).join("cgroup.kill"), "1");
    }
    kill_process_group(getpgrp(), Signal::KILL)?;
    Err("process-group SIGKILL returned".into())
}

fn decode_metadata(
    encoded: &mut impl Iterator<Item = OsString>,
) -> LauncherResult<(Option<OsString>, Vec<OsString>, WorkerEnvironment)> {
    match encoded.next() {
        Some(version) if version == WORKER_METADATA_VERSION => {}
        Some(_) => return Err("unsupported worker metadata version".into()),
        None => return Err("missing worker metadata version".into()),
    }
    let cgroup_path = match encoded.next().as_deref() {
        Some(value) if value == "0" => None,
        Some(value) if value == "1" => {
            let mut path_total = 0;
            let path = decode_item(encoded.next(), &mut path_total)?;
            if !Path::new(&path).is_absolute() {
                return Err("worker cgroup path is not absolute".into());
            }
            Some(path)
        }
        Some(_) => return Err("invalid worker cgroup metadata".into()),
        None => return Err("missing worker cgroup metadata".into()),
    };
    let argument_count = parse_count(encoded.next(), "argument", MAX_WORKER_ARGUMENTS)?;
    let mut total = 0_usize;
    let mut arguments = Vec::with_capacity(argument_count);
    for _ in 0..argument_count {
        arguments.push(decode_item(encoded.next(), &mut total)?);
    }
    let environment_count =
        parse_count(encoded.next(), "environment entry", MAX_WORKER_ENVIRONMENT)?;
    let mut environment = Vec::with_capacity(environment_count);
    for _ in 0..environment_count {
        let key = decode_item(encoded.next(), &mut total)?;
        let value = decode_item(encoded.next(), &mut total)?;
        if key.as_bytes().is_empty()
            || key.as_bytes().contains(&0)
            || key.as_bytes().contains(&b'=')
            || value.as_bytes().contains(&0)
        {
            return Err("invalid worker environment entry".into());
        }
        environment.push((key, value));
    }
    if total > MAX_WORKER_METADATA_BYTES {
        return Err("aggregate worker metadata exceeds bound".into());
    }
    Ok((cgroup_path, arguments, environment))
}

fn parse_count(encoded: Option<OsString>, label: &str, maximum: usize) -> LauncherResult<usize> {
    let count = encoded
        .and_then(|value| value.into_string().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| format!("missing or invalid {label} count"))?;
    if count > maximum {
        return Err(format!("{label} count exceeds bound").into());
    }
    Ok(count)
}

fn decode_item(encoded: Option<OsString>, total: &mut usize) -> LauncherResult<OsString> {
    let encoded = encoded.ok_or("missing worker metadata item")?;
    let bytes = encoded.as_bytes();
    if bytes.len() % 2 != 0 || bytes.len() / 2 > MAX_WORKER_METADATA_ITEM_BYTES {
        return Err("invalid or oversized worker metadata item".into());
    }
    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        decoded.push((decode_nibble(pair[0])? << 4) | decode_nibble(pair[1])?);
    }
    *total = total.saturating_add(decoded.len());
    Ok(OsString::from_vec(decoded))
}

fn decode_nibble(byte: u8) -> LauncherResult<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err("worker metadata is not lowercase hexadecimal".into()),
    }
}
