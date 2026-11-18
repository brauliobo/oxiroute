use super::{Op, script, unified};

fn lines(count: usize) -> String {
    (1..=count)
        .map(|line| ["line ", &line.to_string(), "\n"].concat())
        .collect()
}

#[test]
fn equal_texts_have_no_diff() {
    assert_eq!(unified("a\nb\n", "a\nb\n", "disk", "candidate"), "");
}

#[test]
fn a_replaced_line_is_shown_with_its_context() {
    let new = lines(10).replace("line 5\n", "five\n");
    assert_eq!(
        unified(&lines(10), &new, "disk", "candidate"),
        "--- disk\n+++ candidate\n@@ -2,7 +2,7 @@\n line 2\n line 3\n line 4\n-line 5\n+five\n line 6\n line 7\n line 8\n"
    );
}

#[test]
fn distant_changes_make_separate_hunks() {
    let new = lines(40)
        .replace("line 2\n", "two\n")
        .replace("line 39\n", "thirty-nine\n");
    let diff = unified(&lines(40), &new, "a", "b");
    assert_eq!(diff.matches("@@ -").count(), 2, "{diff}");
    assert!(diff.contains("-line 2\n+two\n"), "{diff}");
    assert!(diff.contains("-line 39\n+thirty-nine\n"), "{diff}");
}

#[test]
fn insertions_and_deletions_at_the_ends() {
    assert_eq!(
        unified("", "a\n", "x", "y"),
        "--- x\n+++ y\n@@ -0,0 +1 @@\n+a\n"
    );
    assert_eq!(
        unified("a\nb\n", "", "x", "y"),
        "--- x\n+++ y\n@@ -1,2 +0,0 @@\n-a\n-b\n"
    );
    assert_eq!(
        unified("b\n", "a\nb\nc\n", "x", "y"),
        "--- x\n+++ y\n@@ -1 +1,3 @@\n+a\n b\n+c\n"
    );
}

#[test]
fn a_missing_final_newline_is_a_difference() {
    assert_eq!(
        unified("a\nb", "a\nb\n", "x", "y"),
        "--- x\n+++ y\n@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+b\n"
    );
}

#[test]
fn the_script_reconstructs_the_new_text_for_shuffled_inputs() {
    let old = "a\nb\nc\nd\ne\nf\ng\n";
    let new = "b\nc\nx\nd\ng\nf\ne\ny\n";
    let diff = unified(old, new, "x", "y");
    let mut rebuilt = Vec::new();
    for line in diff.lines().skip(2) {
        if let Some(' ' | '+') = line.chars().next() {
            rebuilt.push(&line[1..]);
        }
    }
    assert_eq!(rebuilt.join("\n") + "\n", new, "{diff}");
}

fn lcg(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state >> 33
}

fn longest_common(old: &[&str], new: &[&str]) -> usize {
    let mut table = vec![vec![0; new.len() + 1]; old.len() + 1];
    for (i, a) in old.iter().enumerate() {
        for (j, b) in new.iter().enumerate() {
            table[i + 1][j + 1] = if a == b {
                table[i][j] + 1
            } else {
                table[i][j + 1].max(table[i + 1][j])
            };
        }
    }
    table[old.len()][new.len()]
}

#[test]
fn the_edit_script_is_a_shortest_one_for_random_texts() {
    let alphabet = ["a\n", "b\n", "c\n", "d\n"];
    let mut state = 7;
    for _ in 0..400 {
        let mut draw = |limit: u64| {
            (0..lcg(&mut state) % limit)
                .map(|_| alphabet[usize::try_from(lcg(&mut state) % 4).unwrap()])
                .collect::<Vec<_>>()
        };
        let (old, new) = (draw(14), draw(14));
        let ops = script(&old, &new);
        let (mut old_at, mut new_at, mut rebuilt) = (0, 0, Vec::new());
        for op in &ops {
            match op {
                Op::Keep => {
                    assert_eq!(old[old_at], new[new_at]);
                    rebuilt.push(old[old_at]);
                    old_at += 1;
                    new_at += 1;
                }
                Op::Delete => old_at += 1,
                Op::Insert => {
                    rebuilt.push(new[new_at]);
                    new_at += 1;
                }
            }
        }
        assert_eq!((old_at, new_at, &rebuilt), (old.len(), new.len(), &new));
        let edits = ops.iter().filter(|op| **op != Op::Keep).count();
        assert_eq!(
            edits,
            old.len() + new.len() - 2 * longest_common(&old, &new)
        );
    }
}
