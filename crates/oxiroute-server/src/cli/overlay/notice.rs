use serde_json::Value;

use crate::cli::{context::Context, error::CliError, global::Output, render};

const OVERLAY: &str = "/api/v1/overlay";

pub(in crate::cli) fn announce(context: &Context<'_>) -> Result<(), CliError> {
    if context.quiet() || matches!(context.output(), Output::Json) {
        return Ok(());
    }
    let overlay = match context.client()?.get(OVERLAY, true) {
        Ok(response) => response.json()?,
        Err(CliError::Status(401 | 403 | 404, _)) => return Ok(()),
        Err(error) => return Err(error),
    };
    if let Some(notice) = describe(&overlay) {
        render::diagnose(&notice);
    }
    Ok(())
}

fn describe(overlay: &Value) -> Option<String> {
    if overlay.get("state").and_then(Value::as_str) == Some("unreadable") {
        return Some(
            "oxiroute: the overlay file is unreadable and was not applied; fix it or run `oxiroute overlay clear`"
                .into(),
        );
    }
    let settings = overlay.get("settings").and_then(Value::as_array)?;
    let with = |status: &str| {
        settings
            .iter()
            .filter(|setting| setting.get("status").and_then(Value::as_str) == Some(status))
            .count()
    };
    (!settings.is_empty()).then(|| {
        format!(
            "oxiroute: persistence overlay: {} setting(s) kept with --persist override the configuration \
             ({} diverged, {} orphan); see `oxiroute overlay show`",
            settings.len(),
            with("diverged"),
            with("orphan"),
        )
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::describe;

    #[test]
    fn an_empty_overlay_says_nothing() {
        assert_eq!(describe(&json!({ "state": "empty", "settings": [] })), None);
    }

    #[test]
    fn kept_settings_are_counted_by_status() {
        let overlay = json!({ "state": "diverged", "settings": [
            { "status": "applied" }, { "status": "diverged" }, { "status": "orphan" },
        ]});
        let notice = describe(&overlay).expect("notice");
        assert!(notice.contains("3 setting(s)"), "{notice}");
        assert!(notice.contains("1 diverged, 1 orphan"), "{notice}");
    }

    #[test]
    fn an_unreadable_overlay_is_called_out() {
        let notice = describe(&json!({ "state": "unreadable", "settings": [] })).expect("notice");
        assert!(notice.contains("unreadable"), "{notice}");
    }
}
