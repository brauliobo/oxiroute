use std::{path::PathBuf, process::ExitCode};

use super::{
    DoctorArgs, certificates, config,
    diagnosis::{self, Check, Diagnosis},
    files,
    listeners::{self, Host},
    unit::{self, Unit},
    upstreams, writes,
};
use crate::cli::{
    context::Context,
    plugin::{self, Report},
    units::now_unix_seconds,
};

const DEFAULT_CONFIG: &str = "oxiroute.kdl";

pub(in crate::cli) fn run(context: &Context<'_>, args: &DoctorArgs) -> ExitCode {
    let report: Report = diagnosis::report(diagnose(context, args));
    plugin::finish(context, args.json, &report)
}

fn daemon_runs(context: &Context<'_>) -> bool {
    context
        .public_client()
        .and_then(|client| client.get("/status.json", false))
        .is_ok()
}

fn diagnose(context: &Context<'_>, args: &DoctorArgs) -> Vec<Diagnosis> {
    let mut diagnoses = Vec::new();
    let unit = unit::find(args.unit.as_deref()).unwrap_or_else(|reason| {
        diagnoses.push(Diagnosis::critical(Check::Unit, "systemd unit", reason));
        None
    });
    let environment = unit
        .as_ref()
        .map_or_else(unit::Environment::of_process, Unit::environment);
    let path = args
        .config
        .clone()
        .or_else(|| unit.as_ref()?.serves.clone())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG));
    let daemon_running = !args.offline && daemon_runs(context);
    if daemon_running {
        diagnoses.push(Diagnosis::ok(
            Check::Daemon,
            "status page",
            "a daemon answers, so the addresses it holds are not reported as in use",
        ));
    }
    diagnoses.push(files::config_file(&path));
    let writes = match config::check(&path) {
        Ok((draft, found)) => {
            diagnoses.extend(found);
            diagnoses.extend(files::of_config(&draft, &environment));
            let host = Host {
                daemon_running,
                grants_privileged_ports: unit.as_ref().map(|unit| unit.grants_privileged_ports),
            };
            diagnoses.extend(listeners::check(&draft, &host));
            diagnoses.extend(upstreams::check(&draft, context.timeouts().connect()));
            diagnoses.extend(certificates::check(&draft, now_unix_seconds()));
            writes::of(&draft, &environment)
        }
        Err(failure) => {
            diagnoses.push(failure);
            Vec::new()
        }
    };
    match &unit {
        Some(unit) => diagnoses.extend(writes::judge(unit, &writes)),
        None => diagnoses.push(Diagnosis::ok(
            Check::Unit,
            "systemd unit",
            "no oxiroute.service was found, so its sandbox checks were skipped; name one with --unit FILE",
        )),
    }
    diagnoses
}
