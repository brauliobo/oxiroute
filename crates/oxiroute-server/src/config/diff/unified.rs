use std::fmt::Write as _;

const CONTEXT: usize = 3;
const MAX_EDIT_DISTANCE: usize = 4_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Op {
    Keep,
    Delete,
    Insert,
}

/// A unified diff of two texts, empty when they are equal. Lines are compared with their line
/// terminator, so a missing final newline is a difference.
pub(crate) fn unified(old: &str, new: &str, old_label: &str, new_label: &str) -> String {
    if old == new {
        return String::new();
    }
    let old_lines = old.split_inclusive('\n').collect::<Vec<_>>();
    let new_lines = new.split_inclusive('\n').collect::<Vec<_>>();
    let ops = script(&old_lines, &new_lines);
    let mut out = format!("--- {old_label}\n+++ {new_label}\n");
    for hunk in hunks(&ops) {
        render_hunk(
            &mut out,
            &ops[hunk.start..hunk.end],
            &old_lines,
            &new_lines,
            &hunk,
        );
    }
    out
}

struct Hunk {
    start: usize,
    end: usize,
    old_start: usize,
    new_start: usize,
}

fn hunks(ops: &[Op]) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut old_at, mut new_at) = (0, 0);
    let mut positions = Vec::with_capacity(ops.len());
    for op in ops {
        positions.push((old_at, new_at));
        old_at += usize::from(*op != Op::Insert);
        new_at += usize::from(*op != Op::Delete);
    }
    let mut index = 0;
    while index < ops.len() {
        if ops[index] == Op::Keep {
            index += 1;
            continue;
        }
        let mut last = index;
        let mut probe = index + 1;
        while probe < ops.len() && probe <= last + 2 * CONTEXT + 1 {
            if ops[probe] != Op::Keep {
                last = probe;
            }
            probe += 1;
        }
        let start = index.saturating_sub(CONTEXT);
        let end = (last + 1 + CONTEXT).min(ops.len());
        let (old_start, new_start) = positions[start];
        hunks.push(Hunk {
            start,
            end,
            old_start,
            new_start,
        });
        index = end;
    }
    hunks
}

fn render_hunk(out: &mut String, ops: &[Op], old: &[&str], new: &[&str], hunk: &Hunk) {
    let old_count = ops.iter().filter(|op| **op != Op::Insert).count();
    let new_count = ops.iter().filter(|op| **op != Op::Delete).count();
    let _ = writeln!(
        out,
        "@@ -{} +{} @@",
        range(hunk.old_start, old_count),
        range(hunk.new_start, new_count)
    );
    let (mut old_at, mut new_at) = (hunk.old_start, hunk.new_start);
    for op in ops {
        let (marker, line) = match op {
            Op::Keep => (' ', old[old_at]),
            Op::Delete => ('-', old[old_at]),
            Op::Insert => ('+', new[new_at]),
        };
        old_at += usize::from(*op != Op::Insert);
        new_at += usize::from(*op != Op::Delete);
        out.push(marker);
        out.push_str(line);
        if !line.ends_with('\n') {
            out.push_str("\n\\ No newline at end of file\n");
        }
    }
}

fn range(start: usize, count: usize) -> String {
    match count {
        1 => (start + 1).to_string(),
        0 => format!("{start},0"),
        _ => format!("{},{count}", start + 1),
    }
}

fn script(old: &[&str], new: &[&str]) -> Vec<Op> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (old_mid, new_mid) = (
        &old[prefix..old.len() - suffix],
        &new[prefix..new.len() - suffix],
    );
    let mut ops = vec![Op::Keep; prefix];
    ops.extend(middle(old_mid, new_mid));
    ops.extend(std::iter::repeat_n(Op::Keep, suffix));
    ops
}

fn middle(old: &[&str], new: &[&str]) -> Vec<Op> {
    if old.is_empty() || new.is_empty() {
        return std::iter::repeat_n(Op::Delete, old.len())
            .chain(std::iter::repeat_n(Op::Insert, new.len()))
            .collect();
    }
    myers(old, new).unwrap_or_else(|| {
        std::iter::repeat_n(Op::Delete, old.len())
            .chain(std::iter::repeat_n(Op::Insert, new.len()))
            .collect()
    })
}

fn myers(old: &[&str], new: &[&str]) -> Option<Vec<Op>> {
    let reach = (old.len() + new.len()).min(MAX_EDIT_DISTANCE);
    let origin = reach + 1;
    let mut furthest = vec![0_usize; 2 * reach + 3];
    let mut trace: Vec<Vec<usize>> = Vec::new();
    let mut found = None;
    'rounds: for edits in 0..=reach {
        for diagonal in (origin - edits..=origin + edits).step_by(2) {
            let down = diagonal == origin - edits
                || (diagonal != origin + edits && furthest[diagonal - 1] < furthest[diagonal + 1]);
            let mut old_at = if down {
                furthest[diagonal + 1]
            } else {
                furthest[diagonal - 1] + 1
            };
            let mut new_at = old_at + origin - diagonal;
            while old_at < old.len() && new_at < new.len() && old[old_at] == new[new_at] {
                old_at += 1;
                new_at += 1;
            }
            furthest[diagonal] = old_at;
            if old_at >= old.len() && new_at >= new.len() {
                found = Some(edits);
                break 'rounds;
            }
        }
        trace.push(furthest[origin - edits..=origin + edits].to_vec());
    }
    let distance = found?;
    let mut ops = Vec::new();
    let (mut old_at, mut new_at) = (old.len(), new.len());
    for edits in (1..=distance).rev() {
        let diagonal = origin + old_at - new_at;
        let previous = &trace[edits - 1];
        let reached = |at: usize| previous[at + edits - 1 - origin];
        let down = diagonal == origin - edits
            || (diagonal != origin + edits && reached(diagonal - 1) < reached(diagonal + 1));
        let previous_old = reached(if down { diagonal + 1 } else { diagonal - 1 });
        let moved_old = if down { previous_old } else { previous_old + 1 };
        let moved_new = moved_old + origin - diagonal;
        while old_at > moved_old && new_at > moved_new {
            ops.push(Op::Keep);
            old_at -= 1;
            new_at -= 1;
        }
        if down {
            ops.push(Op::Insert);
            new_at -= 1;
        } else {
            ops.push(Op::Delete);
            old_at -= 1;
        }
    }
    ops.extend(std::iter::repeat_n(Op::Keep, old_at));
    ops.reverse();
    Some(ops)
}

#[cfg(test)]
mod tests;
