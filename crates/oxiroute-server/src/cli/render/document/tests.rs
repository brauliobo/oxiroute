use super::{Block, Document, Field};

fn pool_rows() -> Document {
    Document::rows(
        "pools",
        vec![
            vec![
                Field::text("name", Some("web")),
                Field::count("active", Some(1_500)),
                Field::bytes("sent", Some(2_048)),
            ],
            vec![
                Field::text("name", Some("postgres")),
                Field::count("active", Some(3)),
                Field::bytes("sent", None),
            ],
        ],
    )
}

#[test]
fn table_aligns_columns_and_right_aligns_numbers() {
    let text = pool_rows().table();
    assert_eq!(
        text.stdout,
        "NAME      ACTIVE  SENT\nweb         1.5k  2.0K\npostgres       3     -\n"
    );
    assert!(text.notices.is_empty());
}

#[test]
fn plain_prints_one_record_per_line_with_unit_suffixes_and_raw_values() {
    assert_eq!(
        pool_rows().plain(),
        "name=web active=1500 sent_bytes=2048\nname=postgres active=3\n"
    );
}

#[test]
fn a_record_prints_labels_aligned_and_one_plain_line() {
    let document = Document::record(
        "generation",
        vec![
            Field::text("last_failure", None),
            Field::revision("active", Some("0123456789abcdef")),
            Field::flag("degraded", Some(false)),
            Field::duration("age", Some(125_000)),
        ],
    );
    assert_eq!(
        document.table().stdout,
        "last failure  -\nactive        0123456789ab\ndegraded      no\nage           2m05s\n"
    );
    assert_eq!(
        document.plain(),
        "active=0123456789abcdef degraded=false age_ms=125000\n"
    );
}

#[test]
fn plain_quotes_values_that_would_break_the_record() {
    let document = Document::record(
        "message",
        vec![
            Field::text("text", Some("two words")),
            Field::text("equals", Some("a=b")),
            Field::text("empty", Some("")),
        ],
    );
    assert_eq!(
        document.plain(),
        "text=\"two words\" equals=\"a=b\" empty=\"\"\n"
    );
}

#[test]
fn empty_rows_print_nothing_but_name_the_missing_things() {
    let document = Document::rows("upstream_pools", Vec::new());
    let text = document.table();
    assert_eq!(text.stdout, "");
    assert_eq!(text.notices, ["no upstream pools"]);
    assert_eq!(document.plain(), "");
}

#[test]
fn several_blocks_are_separated_and_named_in_plain() {
    let document = Document::new(vec![
        Block::Record {
            name: "status".into(),
            fields: vec![Field::text("state", Some("healthy"))],
        },
        Block::Rows {
            name: "listeners".into(),
            rows: vec![vec![Field::text("name", Some("web"))]],
        },
    ]);
    assert_eq!(document.table().stdout, "state  healthy\n\nNAME\nweb\n");
    assert_eq!(
        document.plain(),
        "record=status state=healthy\nrecord=listeners name=web\n"
    );
}

#[test]
fn notices_follow_the_block_notices() {
    let document = pool_rows().noting(Some("showing 2 of 9 pools".to_owned()));
    assert_eq!(document.table().notices, ["showing 2 of 9 pools"]);
}

#[test]
fn timestamps_print_as_utc_and_stay_numeric_in_plain() {
    let document = Document::record(
        "event",
        vec![Field::timestamp("time", Some(1_795_018_962_349))],
    );
    assert_eq!(document.table().stdout, "time  2026-11-18T16:22:42Z\n");
    assert_eq!(document.plain(), "time_unix_ms=1795018962349\n");
}
