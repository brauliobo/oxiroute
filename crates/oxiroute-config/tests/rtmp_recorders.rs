use oxiroute_config::{Config, ConfigError, load_lua, render_lua, validate_config};

const DEFAULT_MAX_QUEUE_MESSAGES: u64 = 256;
const DEFAULT_MAX_QUEUE_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_MAX_STORAGE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const DEFAULT_MAX_STORAGE_FILES: u64 = 10_000;
const DEFAULT_MAX_ACTIVE_RECORDERS: u64 = 8;

fn config(services: &str) -> String {
    format!(
        r"return {{
  version = 1,
  listeners = {{}},
  rtmp_services = {{
{services}
  }},
}}"
    )
}

fn service(name: &str, applications: &str) -> String {
    format!(
        r#"    {{
      name = "{name}",
      applications = {{
{applications}
      }},
    }},"#
    )
}

fn application(name: &str, live: bool, recorders: &str) -> String {
    format!(
        r#"        {{
          name = "{name}",
          live = {live},
          recorders = {{
{recorders}
          }},
        }},"#
    )
}

fn recorder(name: &str, root_directory: &str, fields: &str) -> String {
    format!(
        r#"            {{
              name = "{name}",
              root_directory = "{root_directory}",
{fields}
            }},"#
    )
}

fn one_recorder_source(live: bool, root_directory: &str, fields: &str) -> String {
    config(&service(
        "live",
        &application("camera", live, &recorder("archive", root_directory, fields)),
    ))
}

fn error(source: &str) -> String {
    load_lua(source)
        .expect_err("configuration must be rejected")
        .to_string()
}

fn first_recorder(source: &str) -> serde_json::Value {
    let config = load_lua(source).expect("configuration with recorder");
    serde_json::to_value(config).expect("serialized configuration")["rtmp_services"][0]
        ["applications"][0]["recorders"][0]
        .clone()
}

#[test]
fn applies_explicit_bounded_recorder_defaults() {
    let recorder = first_recorder(&one_recorder_source(
        true,
        "/var/lib/oxiroute/recordings",
        "",
    ));

    assert_eq!(recorder["name"], "archive");
    assert_eq!(recorder["start"], "continuous");
    assert_eq!(recorder["root_directory"], "/var/lib/oxiroute/recordings");
    assert_eq!(recorder["suffix_template"], ".flv");
    assert_eq!(recorder["append_unix_seconds"], false);
    assert_eq!(recorder["rotation_interval_ms"], serde_json::Value::Null);
    assert_eq!(recorder["max_queue_messages"], DEFAULT_MAX_QUEUE_MESSAGES);
    assert_eq!(recorder["max_queue_bytes"], DEFAULT_MAX_QUEUE_BYTES);
    assert_eq!(recorder["shutdown_timeout_ms"], DEFAULT_SHUTDOWN_TIMEOUT_MS);
    assert_eq!(recorder["max_storage_bytes"], DEFAULT_MAX_STORAGE_BYTES);
    assert_eq!(recorder["max_storage_files"], DEFAULT_MAX_STORAGE_FILES);
    assert_eq!(
        recorder["max_active_recorders"],
        DEFAULT_MAX_ACTIVE_RECORDERS
    );
}

#[test]
fn applies_the_same_defaults_and_normalization_to_json() {
    let mut config: Config = serde_json::from_value(serde_json::json!({
        "version": 1,
        "listeners": [],
        "rtmp_services": [{
            "name": "live",
            "applications": [{
                "name": "camera",
                "live": true,
                "recorders": [{
                    "name": "archive",
                    "root_directory": "/var//lib///oxiroute/recordings"
                }]
            }]
        }]
    }))
    .expect("JSON recorder policy");
    validate_config(&mut config).expect("normalized JSON recorder policy");
    let recorder = serde_json::to_value(config).expect("serialized configuration")["rtmp_services"]
        [0]["applications"][0]["recorders"][0]
        .clone();

    assert_eq!(recorder["start"], "continuous");
    assert_eq!(recorder["root_directory"], "/var/lib/oxiroute/recordings");
    assert_eq!(recorder["suffix_template"], ".flv");
    assert_eq!(recorder["rotation_interval_ms"], serde_json::Value::Null);
    assert_eq!(recorder["max_storage_bytes"], DEFAULT_MAX_STORAGE_BYTES);
}

#[test]
fn loads_normalizes_and_deterministically_renders_the_complete_policy() {
    let source = one_recorder_source(
        true,
        "/var//lib///oxiroute/recordings",
        r#"              start = "manual",
              suffix_template = "-%Y-%m-%dT%H-%M-%S-%%.flv",
              append_unix_seconds = true,
              rotation_interval_ms = 60000,
              max_queue_messages = 128,
              max_queue_bytes = 1048576,
              shutdown_timeout_ms = 3000,
              max_storage_bytes = 1073741824,
              max_storage_files = 4096,
              max_active_recorders = 4,"#,
    );
    let loaded = load_lua(&source).expect("complete recorder policy");
    let rendered = render_lua(&loaded).expect("rendered recorder policy");
    let reloaded = load_lua(&rendered).expect("rendered policy reload");

    assert_eq!(reloaded, loaded);
    assert_eq!(render_lua(&reloaded).expect("second render"), rendered);
    for field in [
        "recorders",
        "name",
        "start",
        "root_directory",
        "suffix_template",
        "append_unix_seconds",
        "rotation_interval_ms",
        "max_queue_messages",
        "max_queue_bytes",
        "shutdown_timeout_ms",
        "max_storage_bytes",
        "max_storage_files",
        "max_active_recorders",
    ] {
        assert!(
            rendered.contains(&format!("{field} =")),
            "renderer omitted {field}"
        );
    }
    assert!(rendered.contains("root_directory = \"/var/lib/oxiroute/recordings\","));
    assert!(rendered.contains("start = \"manual\","));
    assert!(rendered.contains("rotation_interval_ms = 60000,"));
}

#[test]
fn accepts_omitted_and_explicit_null_rotation_without_aliases() {
    for rotation in ["", "              rotation_interval_ms = null,"] {
        let recorder = first_recorder(&one_recorder_source(
            true,
            "/var/lib/oxiroute/recordings",
            rotation,
        ));
        assert_eq!(recorder["rotation_interval_ms"], serde_json::Value::Null);
    }

    let source = one_recorder_source(
        true,
        "/var/lib/oxiroute/recordings",
        "              rotation_interval = 60000,",
    );
    assert!(error(&source).contains("unknown field `rotation_interval`"));

    let rendered = render_lua(
        &load_lua(&one_recorder_source(
            true,
            "/var/lib/oxiroute/recordings",
            "",
        ))
        .expect("default rotation"),
    )
    .expect("render default rotation");
    assert!(rendered.contains("rotation_interval_ms = null,"));
}

#[test]
fn requires_recorders_to_belong_to_live_applications() {
    let source = one_recorder_source(false, "/var/lib/oxiroute/recordings", "");
    let error = error(&source);

    assert!(error.contains("RTMP recorder `archive`"));
    assert!(error.contains("application `camera`"));
    assert!(error.contains("requires `live = true`"));
}

#[test]
fn applies_existing_name_rules_with_application_local_uniqueness() {
    for name in ["", "  ", " archive ", "archive\\nsecondary"] {
        let source = config(&service(
            "live",
            &application(
                "camera",
                true,
                &recorder(name, "/var/lib/oxiroute/recordings", ""),
            ),
        ));
        let error = error(&source);
        assert!(error.contains("RTMP recorder"), "{name:?}: {error}");
    }

    let duplicate = recorder("archive", "/var/lib/oxiroute/first", "")
        + &recorder("archive", "/var/lib/oxiroute/second", "");
    let source = config(&service("live", &application("camera", true, &duplicate)));
    assert!(error(&source).contains("duplicate RTMP recorder name `archive`"));

    let applications = application(
        "first",
        true,
        &recorder("archive", "/var/lib/oxiroute/first", ""),
    ) + &application(
        "second",
        true,
        &recorder("archive", "/var/lib/oxiroute/second", ""),
    );
    load_lua(&config(&service("live", &applications)))
        .expect("recorder names are application-local");
}

#[test]
fn bounds_recorders_per_application_and_globally() {
    let eight = (0..8)
        .map(|index| recorder(&format!("recorder-{index}"), "/var/lib/oxiroute/shared", ""))
        .collect::<String>();
    load_lua(&config(&service(
        "live",
        &application("camera", true, &eight),
    )))
    .expect("eight recorders per application");

    let nine = eight + &recorder("recorder-8", "/var/lib/oxiroute/shared", "");
    let source = config(&service("live", &application("camera", true, &nine)));
    assert!(error(&source).contains("8-recorder limit"));

    let applications = (0..33)
        .map(|application_index| {
            let count = if application_index == 32 { 1 } else { 8 };
            let recorders = (0..count)
                .map(|recorder_index| {
                    recorder(
                        &format!("recorder-{recorder_index}"),
                        "/var/lib/oxiroute/shared",
                        "",
                    )
                })
                .collect::<String>();
            application(
                &format!("application-{application_index}"),
                true,
                &recorders,
            )
        })
        .collect::<String>();
    let source = config(&service("live", &applications));
    assert!(error(&source).contains("256-RTMP-recorder limit"));
}

#[test]
fn bounds_distinct_normalized_recording_roots() {
    let applications = (0..64)
        .map(|index| {
            application(
                &format!("application-{index}"),
                true,
                &recorder("archive", &format!("/var/lib/oxiroute/root-{index}"), ""),
            )
        })
        .collect::<String>();
    load_lua(&config(&service("live", &applications))).expect("64 recording roots");

    let source = config(&service(
        "live",
        &(applications
            + &application(
                "application-64",
                true,
                &recorder("archive", "/var/lib/oxiroute/root-64", ""),
            )),
    ));
    assert!(error(&source).contains("64-recording-root limit"));
}

#[test]
fn validates_and_normalizes_recording_roots_lexically() {
    let boundary = format!("/{}", "a".repeat(4_095));
    load_lua(&one_recorder_source(true, &boundary, "")).expect("4096-byte root path");

    for root in [
        "var/lib/recordings".to_owned(),
        "/".to_owned(),
        "/var/lib/recordings/".to_owned(),
        "/var/./recordings".to_owned(),
        "/var/../recordings".to_owned(),
        r"/var/lib/\0recordings".to_owned(),
        format!("/{}", "a".repeat(4_096)),
    ] {
        let source = one_recorder_source(true, &root, "");
        let error = error(&source);
        assert!(
            error.contains("invalid `root_directory`"),
            "{root:?}: {error}"
        );
    }

    let recorder = first_recorder(&one_recorder_source(
        true,
        "/var//lib///oxiroute/recordings",
        "",
    ));
    assert_eq!(recorder["root_directory"], "/var/lib/oxiroute/recordings");
}

#[test]
fn enforces_the_recording_path_policy_suffix_grammar() {
    let boundary = "é".repeat(64);
    for suffix in ["", ".flv", "-%Y-%m-%dT%H-%M-%S-%%.flv", &boundary] {
        let fields = format!("              suffix_template = \"{suffix}\",");
        load_lua(&one_recorder_source(
            true,
            "/var/lib/oxiroute/recordings",
            &fields,
        ))
        .unwrap_or_else(|error| panic!("valid suffix {suffix:?}: {error}"));
    }

    let too_long = format!("{boundary}x");
    for suffix in ["/.flv", r"\\.flv", ".flv\\0", "%", "%Q", "%é", &too_long] {
        let fields = format!("              suffix_template = \"{suffix}\",");
        let source = one_recorder_source(true, "/var/lib/oxiroute/recordings", &fields);
        let error = error(&source);
        assert!(
            error.contains("invalid `suffix_template`"),
            "{suffix:?}: {error}"
        );
    }
}

#[test]
fn enforces_nonzero_bounded_recorder_limits() {
    let limits = [
        ("max_queue_messages", 65_536_u64),
        ("max_queue_bytes", 1024 * 1024 * 1024),
        ("shutdown_timeout_ms", 60_000),
        ("max_storage_bytes", 1024 * 1024 * 1024 * 1024),
        ("max_storage_files", 1_000_000),
        ("max_active_recorders", 256),
    ];

    for (field, maximum) in limits {
        for value in [0, maximum + 1, 9_007_199_254_740_992] {
            let fields = format!("              {field} = {value},");
            let source = one_recorder_source(true, "/var/lib/oxiroute/recordings", &fields);
            let error = error(&source);
            assert!(error.contains(&format!("invalid `{field}`")), "{error}");
        }
    }

    for value in [0, 2_147_483_648_u64] {
        let fields = format!("              rotation_interval_ms = {value},");
        let source = one_recorder_source(true, "/var/lib/oxiroute/recordings", &fields);
        assert!(error(&source).contains("invalid `rotation_interval_ms`"));
    }
    load_lua(&one_recorder_source(
        true,
        "/var/lib/oxiroute/recordings",
        "              rotation_interval_ms = 2147483647,",
    ))
    .expect("maximum rotation interval");
}

#[test]
fn requires_the_queue_byte_bound_to_fit_within_storage() {
    let source = one_recorder_source(
        true,
        "/var/lib/oxiroute/recordings",
        r"              max_queue_bytes = 1048577,
              max_storage_bytes = 1048576,",
    );
    let error = error(&source);

    assert!(error.contains("max_queue_bytes"));
    assert!(error.contains("max_storage_bytes"));
}

#[test]
fn shared_normalized_roots_require_identical_storage_limits_only() {
    let first = recorder(
        "first",
        "/var/lib/oxiroute/shared",
        r#"              start = "continuous",
              suffix_template = ".flv",
              max_queue_messages = 64,
              rotation_interval_ms = 1000,
              max_storage_bytes = 1073741824,
              max_storage_files = 1000,
              max_active_recorders = 4,"#,
    );
    let second = recorder(
        "second",
        "/var//lib///oxiroute/shared",
        r#"              start = "manual",
              suffix_template = "-%Y.flv",
              append_unix_seconds = true,
              max_queue_messages = 32,
              max_queue_bytes = 524288,
              rotation_interval_ms = null,
              max_storage_bytes = 1073741824,
              max_storage_files = 1000,
              max_active_recorders = 4,"#,
    );
    load_lua(&config(&service(
        "live",
        &application("camera", true, &(first.clone() + &second)),
    )))
    .expect("shared root permits distinct worker and path policies");

    for field in [
        "max_storage_bytes",
        "max_storage_files",
        "max_active_recorders",
    ] {
        let mismatched = second.replace(
            &format!("{field} = {}", storage_value(field)),
            &format!("{field} = {}", storage_value(field) + 1),
        );
        let source = config(&service(
            "live",
            &application("camera", true, &(first.clone() + &mismatched)),
        ));
        let error = error(&source);
        assert!(error.contains("shared recording root"), "{field}: {error}");
        assert!(
            error.contains("identical storage limits"),
            "{field}: {error}"
        );
    }
}

#[test]
fn retains_the_existing_source_and_render_size_bounds() {
    let oversized_name = "x".repeat(1024 * 1024);
    let source = config(&service(
        "live",
        &application(
            "camera",
            true,
            &recorder(&oversized_name, "/var/lib/oxiroute/recordings", ""),
        ),
    ));
    assert!(matches!(
        load_lua(&source),
        Err(ConfigError::SourceTooLarge)
    ));

    let mut config = load_lua(&one_recorder_source(
        true,
        "/var/lib/oxiroute/recordings",
        "",
    ))
    .expect("bounded recorder configuration");
    config.rtmp_services[0].applications[0].recorders[0].name = oversized_name;
    assert!(matches!(
        render_lua(&config),
        Err(ConfigError::SourceTooLarge)
    ));
}

const fn storage_value(field: &str) -> u64 {
    match field.as_bytes() {
        b"max_storage_bytes" => 1_073_741_824,
        b"max_storage_files" => 1_000,
        b"max_active_recorders" => 4,
        _ => unreachable!(),
    }
}
