use std::{
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
};

use clap::{Subcommand, ValueEnum};
use serde_json::json;

use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    net::encode,
    parse::nonempty_target,
    view::View,
};

const RECORDINGS: &str = "/api/v1/rtmp/recordings";

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Status {
    Recording,
    Complete,
    Partial,
}

impl Status {
    const fn name(self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Complete => "complete",
            Self::Partial => "partial",
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum RecordingCommand {
    /// List the stored recordings, newest first, with their stream, status, size and duration.
    List {
        /// Only recordings of this stream name.
        #[arg(long, value_parser = nonempty_target)]
        stream: Option<String>,
        /// Only recordings written by this recorder.
        #[arg(long, value_parser = nonempty_target)]
        recorder: Option<String>,
        /// Only recordings with this status.
        #[arg(long, value_enum)]
        status: Option<Status>,
        /// Most recordings to print, 1 to 1000.
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
        /// Continue after the recording the previous page ended with, as it printed.
        #[arg(long, value_parser = nonempty_target)]
        cursor: Option<String>,
    },
    /// Show one recording.
    Show {
        /// Id of the recording, as printed by `rtmp recording list`; it survives restarts.
        #[arg(value_parser = nonempty_target)]
        id: String,
    },
    /// Delete a recording that is not being written.
    Delete {
        /// Id of the recording.
        #[arg(value_parser = nonempty_target)]
        id: String,
        /// Confirm the deletion, which cannot be undone.
        #[arg(long, required = true)]
        yes: bool,
    },
    /// Download the recording file.
    Download {
        /// Id of the recording.
        #[arg(value_parser = nonempty_target)]
        id: String,
        /// File to write, or `-` for standard output; default: the recording's file name in this directory.
        #[arg(long, value_name = "FILE")]
        to: Option<PathBuf>,
        /// Replace the file when it exists.
        #[arg(long)]
        force: bool,
    },
    /// Show how full each recording root is and how it frees space: quota, retention and evictions.
    Quota,
}

impl Execute for RecordingCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::List {
                stream,
                recorder,
                status,
                limit,
                cursor,
            } => context.show(
                &list_path(
                    stream.as_deref(),
                    recorder.as_deref(),
                    *status,
                    *limit,
                    cursor.as_deref(),
                ),
                View::RtmpRecordings,
            ),
            Self::Show { id } => context.show(&item(id), View::RtmpRecording),
            Self::Delete { id, .. } => {
                let response = context
                    .client()?
                    .request("DELETE", &item(id), &[], &[], true)?;
                context.print_response(&response, View::Generic)
            }
            Self::Download { id, to, force } => download(context, id, to.as_deref(), *force),
            Self::Quota => context.show(&format!("{RECORDINGS}?limit=1"), View::RtmpQuota),
        }
    }
}

fn item(id: &str) -> String {
    format!("{RECORDINGS}/{}", encode(id))
}

pub(super) fn list_path(
    stream: Option<&str>,
    recorder: Option<&str>,
    status: Option<Status>,
    limit: u16,
    cursor: Option<&str>,
) -> String {
    let mut path = format!("{RECORDINGS}?limit={limit}");
    for (name, value) in [
        ("stream", stream),
        ("recorder", recorder),
        ("status", status.map(Status::name)),
        ("cursor", cursor),
    ] {
        if let Some(value) = value {
            let _ = write!(path, "&{name}={}", encode(value));
        }
    }
    path
}

fn download(
    context: &Context<'_>,
    id: &str,
    to: Option<&Path>,
    force: bool,
) -> Result<(), CliError> {
    let client = context.client()?;
    let name = || -> Result<String, CliError> {
        let recording = client.get(&item(id), true)?.json()?;
        let file = recording["name"]
            .as_str()
            .and_then(|name| Path::new(name).file_name())
            .and_then(|name| name.to_str())
            .ok_or_else(|| CliError::Remote("the recording has no file name".into()))?;
        Ok(file.to_owned())
    };
    let target = match to {
        Some(path) if path == Path::new("-") => None,
        Some(path) => Some(path.to_owned()),
        None => Some(PathBuf::from(name()?)),
    };
    let mut body = client.stream(
        &format!("{}/content", item(id)),
        "application/octet-stream",
        &[],
        context.timeouts().answer(),
    )?;
    let Some(target) = target else {
        return io::copy(&mut body, &mut io::stdout().lock())
            .map(drop)
            .map_err(CliError::Io);
    };
    if !force && target.exists() {
        return Err(CliError::Local(format!(
            "{} exists; pass --force to replace it",
            target.display()
        )));
    }
    let partial = target.with_file_name(format!(
        ".{}.partial",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("recording")
    ));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .and_then(|mut file| {
            let bytes = io::copy(&mut body, &mut file)?;
            file.flush()?;
            file.sync_all()?;
            Ok(bytes)
        })
        .and_then(|bytes| fs::rename(&partial, &target).map(|()| bytes))
        .inspect_err(|_| drop(fs::remove_file(&partial)))
        .map_err(|error| CliError::Local(format!("cannot write {}: {error}", target.display())))?;
    context.print(
        &json!({ "id": id, "file": target.display().to_string(), "bytes": written }),
        View::Download,
    )
}
