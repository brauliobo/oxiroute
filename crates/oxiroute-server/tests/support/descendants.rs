#![allow(dead_code, clippy::missing_panics_doc, clippy::must_use_candidate)]

use std::fs;

use rustix::process::{Pid, Signal, kill_process};

fn children(pid: u32) -> Vec<u32> {
    let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    let lists = tasks
        .flatten()
        .filter_map(|task| fs::read_to_string(task.path().join("children")).ok());
    let lists = lists.map(|list| {
        list.split_whitespace()
            .filter_map(|pid| pid.parse().ok())
            .collect::<Vec<u32>>()
    });
    lists.flatten().collect()
}

fn signal(pid: u32, signal: Signal) {
    if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
        let _ = kill_process(pid, signal);
    }
}

/// `root` and every process below it, frozen as they are found so that none can start another one meanwhile.
fn frozen_tree(root: u32) -> Vec<u32> {
    let mut tree = vec![root];
    let mut next = 0;
    while let Some(&pid) = tree.get(next) {
        signal(pid, Signal::STOP);
        tree.extend(children(pid));
        next += 1;
    }
    tree
}

/// Kills the process `root` started: its workers, which live in groups of their own, die with it.
pub fn kill_tree(root: u32) {
    for pid in frozen_tree(root) {
        signal(pid, Signal::KILL);
    }
}
