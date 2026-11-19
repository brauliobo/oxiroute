use super::{Chip, Detail, word};
use crate::{
    cli::{
        top::{
            history::Sampling,
            model::{Cell, Column, Row, Table, Tone},
            source::Sample,
        },
        units,
    },
    observability::stats_page::model::{GenerationStats, RetiredGeneration},
};

pub(super) const COLUMNS: &[Column] = &[
    Column::text("name", "NAME", 1),
    Column::text("state", "STATE", 1),
    Column::text("revision", "REVISION", 2),
    Column::number("age", "AGE", 1),
    Column::number("deadline", "DEADLINE", 1),
    Column::number("inflight", "IN-FLIGHT", 1).summed(),
];

const ACTIVE: &str = "generation:active";

pub(super) fn table(sampling: &Sampling) -> Table {
    let generation = &sampling.stats().generation;
    let mut rows = vec![active_row(generation)];
    for retired in &generation.retired {
        let parent = rows.len();
        rows.push(retired_row(retired));
        for (kind, count) in &retired.in_flight_by_kind {
            let cells = vec![
                Cell::text(kind),
                Cell::missing(),
                Cell::missing(),
                Cell::missing(),
                Cell::missing(),
                Cell::plain(*count),
            ];
            let key = format!("retired:{}/{kind}", retired.revision);
            rows.push(Row::new(key, cells, &[kind, &retired.revision]).under(parent, 1));
        }
    }
    Table::new(COLUMNS, rows)
}

fn active_row(generation: &GenerationStats) -> Row {
    let (state, tone) = match (generation.accepting, generation.degraded) {
        (_, true) => ("degraded", Tone::Bad),
        (true, false) => ("accepting", Tone::Good),
        (false, false) => ("not accepting", Tone::Warn),
    };
    let name = format!("#{} active", generation.number);
    let revision = generation.active_revision.as_deref();
    let cells = vec![
        Cell::text(&name),
        Cell::text(state).toned(tone),
        Cell::optional_text(revision),
        Cell::duration(generation.age_ms),
        Cell::missing(),
        Cell::missing(),
    ];
    Row::new(ACTIVE, cells, &[&name, revision.unwrap_or_default()])
}

fn retired_row(retired: &RetiredGeneration) -> Row {
    let cells = vec![
        Cell::text(format!("retired {}", retired.revision)),
        Cell::text(word(&retired.state)).toned(Tone::Warn),
        Cell::text(&retired.revision),
        Cell::duration(retired.age_seconds * 1_000),
        Cell::duration(retired.deadline_remaining_seconds * 1_000),
        Cell::plain(retired.in_flight),
    ];
    Row::new(
        format!("retired:{}", retired.revision),
        cells,
        &[&retired.revision],
    )
}

pub(super) fn summary(sampling: &Sampling) -> Vec<Chip> {
    let generation = &sampling.stats().generation;
    let draining = generation.retired.len();
    let in_flight: u64 = generation
        .retired
        .iter()
        .map(|retired| retired.in_flight)
        .sum();
    vec![
        Chip::new("generation", format!("#{}", generation.number)),
        Chip::new("accepting", if generation.accepting { "yes" } else { "no" }).toned(
            if generation.accepting {
                Tone::Good
            } else {
                Tone::Warn
            },
        ),
        Chip::new(
            "draining",
            format!("{draining}/{}", generation.max_retired_generations),
        ),
        Chip::new("draining in flight", in_flight.to_string()),
        Chip::new("failures", generation.failures.get().to_string()).toned(
            if generation.failures.get() > 0 {
                Tone::Bad
            } else {
                Tone::Dim
            },
        ),
    ]
}

pub(super) fn detail(sampling: &Sampling, key: &str) -> Detail {
    let generation = &sampling.stats().generation;
    if key != ACTIVE {
        return Detail::default().with("kind", "retired generation, draining its connections");
    }
    let mut detail = Detail::default()
        .with("number", generation.number.to_string())
        .with("prepares", generation.prepares.get().to_string())
        .with("activations", generation.activations.get().to_string())
        .with("rejections", generation.config_rejections.get().to_string())
        .with("failures", generation.failures.get().to_string())
        .with("rollbacks", generation.rollbacks.get().to_string());
    for (label, value) in [
        ("active", &generation.active_revision),
        ("candidate", &generation.candidate_revision),
        ("previous", &generation.previous_revision),
        ("on disk", &generation.disk_revision),
        ("quarantined", &generation.quarantined_revision),
        ("last failure", &generation.last_failure),
    ] {
        if let Some(value) = value {
            detail = detail.with(label, value);
        }
    }
    if let Some(rejection) = &generation.last_rejection {
        detail = detail.with("last rejection", units::timestamp(rejection.at_unix_ms));
        for diagnostic in &rejection.diagnostics {
            detail = detail.with(
                "diagnostic",
                format!("{}: {}", diagnostic.code, diagnostic.message),
            );
        }
    }
    detail
}

pub(super) fn badge(sample: &Sample) -> (String, Tone) {
    let generation = &sample.snapshot.generation;
    match generation.retired.len() {
        0 => (format!("#{}", generation.number), Tone::Dim),
        draining => (format!("+{draining}"), Tone::Warn),
    }
}
