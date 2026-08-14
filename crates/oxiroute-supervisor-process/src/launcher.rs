//! Shared implementation of the hardened Linux worker launcher binary.

use std::{
    env,
    ffi::OsString,
    fs::{self, File},
    io,
    os::{
        fd::AsFd as _,
        unix::ffi::{OsStrExt as _, OsStringExt as _},
    },
    path::Path,
    process::{Command, ExitCode, Stdio},
};

use rustix::{
    fs::OFlags,
    process::{Signal, getpgrp, kill_process_group},
};

use crate::{
    MAX_WORKER_ARGUMENTS, MAX_WORKER_ENVIRONMENT, MAX_WORKER_METADATA_ITEM_BYTES,
    WORKER_METADATA_VERSION, WorkerMetadata, cgroup,
};

type LauncherResult<T> = Result<T, Box<dyn std::error::Error>>;

struct LauncherMetadata {
    worker: WorkerMetadata,
    cgroup_path: Option<OsString>,
}

/// Runs the production launcher contract and maps failures to a process exit status.
#[must_use]
pub fn run() -> ExitCode {
    match launch() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("oxiroute worker launcher: {error}");
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

    // This audited dependency uses Linux close_range(CLOSE_RANGE_CLOEXEC) with an fd-iteration
    // fallback. The launcher is single-threaded, and validation below makes failure fail closed.
    close_fds::set_fds_cloexec(3, &[]);
    verify_cloexec_from(3)?;

    let metadata = decode_metadata(&mut encoded)?;
    if let Some(path) = metadata.cgroup_path.as_deref() {
        cgroup::attach_current(Path::new(path))?;
    }
    let (arguments, environment) = metadata.worker.into_parts();
    let mut child = Command::new(worker)
        .args(arguments)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::inherit())
        .spawn()?;

    // Only the worker keeps the control endpoint. The launcher remains as a stable, unreaped group
    // leader and waits for the worker before killing all remaining in-group descendants and itself.
    let null = File::open("/dev/null")?;
    rustix::stdio::dup2_stdin(null.as_fd())?;
    let _worker_status = child.wait()?;
    if let Some(path) = metadata.cgroup_path.as_deref() {
        let _ = cgroup::kill_at(Path::new(path));
    }
    kill_process_group(getpgrp(), Signal::KILL)?;
    Err("process-group SIGKILL returned without terminating launcher".into())
}

fn decode_metadata(
    encoded: &mut impl Iterator<Item = OsString>,
) -> LauncherResult<LauncherMetadata> {
    match encoded.next() {
        Some(version) if version == WORKER_METADATA_VERSION => {}
        Some(_) => return Err("unsupported worker metadata version".into()),
        None => return Err("missing worker metadata version".into()),
    }

    let cgroup_path = match encoded.next().as_deref() {
        Some(value) if value == "0" => None,
        Some(value) if value == "1" => {
            let path = decode_item(encoded.next())?;
            if !Path::new(&path).is_absolute() {
                return Err("worker cgroup path is not absolute".into());
            }
            Some(path)
        }
        Some(_) => return Err("invalid worker cgroup metadata".into()),
        None => return Err("missing worker cgroup metadata".into()),
    };

    let argument_count = parse_count(encoded.next(), "argument", MAX_WORKER_ARGUMENTS)?;
    let mut arguments = Vec::with_capacity(argument_count);
    for _ in 0..argument_count {
        arguments.push(decode_item(encoded.next())?);
    }

    let environment_count =
        parse_count(encoded.next(), "environment entry", MAX_WORKER_ENVIRONMENT)?;
    let mut environment = Vec::with_capacity(environment_count);
    for _ in 0..environment_count {
        let key = decode_item(encoded.next())?;
        let value = decode_item(encoded.next())?;
        environment.push((key, value));
    }
    let metadata = WorkerMetadata::new(arguments, environment)?;
    if encoded.next().is_some() {
        return Err("trailing launcher metadata".into());
    }
    Ok(LauncherMetadata {
        worker: metadata,
        cgroup_path,
    })
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

fn decode_item(encoded: Option<OsString>) -> LauncherResult<OsString> {
    let encoded = encoded.ok_or("missing worker metadata item")?;
    let bytes = encoded.as_bytes();
    if bytes.len() % 2 != 0 || bytes.len() / 2 > MAX_WORKER_METADATA_ITEM_BYTES {
        return Err("invalid or oversized worker metadata item".into());
    }
    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        decoded.push((decode_nibble(pair[0])? << 4) | decode_nibble(pair[1])?);
    }
    Ok(OsString::from_vec(decoded))
}

fn decode_nibble(byte: u8) -> LauncherResult<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err("worker metadata is not lowercase hexadecimal".into()),
    }
}

fn verify_cloexec_from(minimum: i32) -> io::Result<()> {
    let mut descriptors = Vec::new();
    for entry in fs::read_dir("/proc/self/fd")? {
        let entry = entry?;
        let Some(fd) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        if fd >= minimum {
            descriptors.push(fd);
        }
    }

    let cloexec = u64::from(OFlags::CLOEXEC.bits());
    for fd in descriptors {
        let path = format!("/proc/self/fdinfo/{fd}");
        let details = match fs::read_to_string(path) {
            Ok(details) => details,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let flags = details
            .lines()
            .find_map(|line| line.strip_prefix("flags:\t"))
            .and_then(|flags| u64::from_str_radix(flags, 8).ok())
            .ok_or_else(|| io::Error::other(format!("fd {fd} has no parseable flags")))?;
        if flags & cloexec == 0 {
            return Err(io::Error::other(format!(
                "fd {fd} remained inheritable after sanitization"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_decoding_uses_shared_argument_validation() {
        let mut encoded = ["v2", "0", "1", "00", "0"].map(OsString::from).into_iter();
        assert!(decode_metadata(&mut encoded).is_err());
    }

    #[test]
    fn launcher_decoding_preserves_non_utf8_metadata_bytes() {
        let mut encoded = ["v2", "0", "1", "ff", "1", "4b4559", "fe"]
            .map(OsString::from)
            .into_iter();
        let (arguments, environment) = decode_metadata(&mut encoded)
            .expect("decode metadata")
            .worker
            .into_parts();

        assert_eq!(arguments[0].as_bytes(), &[0xff]);
        assert_eq!(environment[0].0.as_bytes(), b"KEY");
        assert_eq!(environment[0].1.as_bytes(), &[0xfe]);
    }

    #[test]
    fn launcher_decoding_requires_an_absolute_cgroup_path() {
        let mut contained = ["v2", "1", "2f63672f77", "0", "0"]
            .map(OsString::from)
            .into_iter();
        assert_eq!(
            decode_metadata(&mut contained)
                .unwrap()
                .cgroup_path
                .as_deref(),
            Some(std::ffi::OsStr::new("/cg/w"))
        );

        let mut relative = ["v2", "1", "63672f77", "0", "0"]
            .map(OsString::from)
            .into_iter();
        assert!(decode_metadata(&mut relative).is_err());
    }

    #[test]
    fn launcher_decoding_requires_exact_metadata_version() {
        let cases = [
            Vec::new(),
            ["0", "0", "0"].map(OsString::from).to_vec(),
            ["v1", "0", "0"].map(OsString::from).to_vec(),
            ["version-two", "0", "0"].map(OsString::from).to_vec(),
        ];

        for encoded in cases {
            assert!(decode_metadata(&mut encoded.into_iter()).is_err());
        }
    }

    #[test]
    fn launcher_decoding_rejects_trailing_metadata() {
        let mut encoded = ["v2", "0", "0", "0", "00"].map(OsString::from).into_iter();
        assert!(decode_metadata(&mut encoded).is_err());
    }

    #[test]
    fn launcher_decoding_preserves_count_and_item_bounds() {
        let mut excessive_count = ["v2", "0", "129", "0"].map(OsString::from).into_iter();
        assert!(decode_metadata(&mut excessive_count).is_err());

        let oversized_item = "00".repeat(MAX_WORKER_METADATA_ITEM_BYTES + 1);
        let mut oversized = [
            OsString::from("v2"),
            OsString::from("0"),
            OsString::from("1"),
            OsString::from(oversized_item),
            OsString::from("0"),
        ]
        .into_iter();
        assert!(decode_metadata(&mut oversized).is_err());
    }
}
