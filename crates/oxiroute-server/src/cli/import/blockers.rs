use std::{fmt::Write as _, fs};

use oxiroute_import::{ImportBlocker, ImportReportEnvelope};

const LISTED: usize = 10;

pub(super) const NGINX_REMEDIATION: &str = "nginx overlays that can resolve blockers: \
--default-access-log-file FILE, --x-accel-controls-absent, --upstream-tls AUTHORITY=SNI, \
--bearer-token-file SERVER_NAME=PATH, --recording-root DIRECTORY, --default-error-server TOKEN, \
--host-timezone TIMEZONE (see `oxiroute import nginx --help`)";

pub(super) fn summary(
    source: &str,
    envelope: &ImportReportEnvelope,
    remediation: Option<&str>,
) -> Option<String> {
    let blockers = &envelope.blockers;
    if blockers.is_empty() {
        return None;
    }
    let mut text = format!(
        "{source}: {} blocker{} remain, so there is no activatable candidate",
        blockers.len(),
        if blockers.len() == 1 { "" } else { "s" },
    );
    for blocker in blockers.iter().take(LISTED) {
        let place = location(envelope, blocker).unwrap_or_else(|| "-".into());
        let _ = write!(text, "\n  {} {place}: {}", blocker.code, blocker.message);
    }
    if blockers.len() > LISTED {
        let _ = write!(
            text,
            "\n  ... and {} more in the report",
            blockers.len() - LISTED
        );
    }
    if let Some(remediation) = remediation {
        let _ = write!(text, "\n{remediation}");
    }
    Some(text)
}

fn location(envelope: &ImportReportEnvelope, blocker: &ImportBlocker) -> Option<String> {
    let origin = blocker.origins.first()?;
    let source = envelope
        .source_graph
        .sources
        .iter()
        .find(|source| source.id == origin.source_id)?;
    let path = source.path.as_deref()?;
    let line = fs::read(path).ok().and_then(|bytes| {
        bytes
            .get(..origin.range.start)
            .map(|before| before.split(|byte| *byte == b'\n').count())
    });
    Some(line.map_or_else(|| path.to_owned(), |line| format!("{path}:{line}")))
}
