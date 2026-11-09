use std::path::Path;

use oxiroute_config::{ConfigDraft, ValidatedConfig};
use oxiroute_config_source::{ConfigFormat, render_config, resolve_source_with_format};
use serde_json::json;

fn validated() -> ValidatedConfig {
    serde_json::from_value::<ConfigDraft>(
        json!({"version": 1, "listeners": [], "listener_workers": 3}),
    )
    .expect("authored configuration")
    .validate()
    .expect("valid configuration")
}

#[test]
fn listener_workers_round_trip_through_every_source_format() {
    let config = validated();
    for format in [
        ConfigFormat::Kdl,
        ConfigFormat::Lua,
        ConfigFormat::Uci,
        ConfigFormat::Hocon,
    ] {
        let rendered = render_config(format, &config).expect("render");
        let resolved = resolve_source_with_format(Path::new("config"), rendered.as_bytes(), format)
            .unwrap_or_else(|error| panic!("{format:?}: {error}"));
        assert_eq!(resolved.config, config, "{format:?}");
    }
}
