use oxiroute_config::{ConfigDraft, RtmpCallbackConfig};

use super::REDACTED_PLACEHOLDER;

pub(super) fn redact_callback_url(url: &str) -> String {
    let (scheme, remainder) = url.split_once("://").unwrap_or(("", url));
    let split = remainder.find(['/', '?']).unwrap_or(remainder.len());
    let (authority, tail) = remainder.split_at(split);
    let (path, query) = tail
        .split_once('?')
        .map_or((tail, None), |(path, query)| (path, Some(query)));
    let mut redacted = String::with_capacity(url.len());
    if !scheme.is_empty() {
        redacted.push_str(scheme);
        redacted.push_str("://");
    }
    if let Some((_, host)) = authority.rsplit_once('@') {
        redacted.push_str(REDACTED_PLACEHOLDER);
        redacted.push('@');
        redacted.push_str(host);
    } else {
        redacted.push_str(authority);
    }
    redacted.push_str(path);
    if let Some(query) = query {
        redacted.push('?');
        redacted.push_str(&redact_query_values(query));
    }
    redacted
}

fn redact_query_values(query: &str) -> String {
    query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, _)) => format!("{name}={REDACTED_PLACEHOLDER}"),
            None => pair.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn urls(callbacks: &RtmpCallbackConfig) -> [&Option<String>; 8] {
    [
        &callbacks.on_connect,
        &callbacks.on_disconnect,
        &callbacks.on_publish,
        &callbacks.on_publish_done,
        &callbacks.on_play,
        &callbacks.on_play_done,
        &callbacks.on_done,
        &callbacks.on_update,
    ]
}

fn urls_mut(callbacks: &mut RtmpCallbackConfig) -> [&mut Option<String>; 8] {
    [
        &mut callbacks.on_connect,
        &mut callbacks.on_disconnect,
        &mut callbacks.on_publish,
        &mut callbacks.on_publish_done,
        &mut callbacks.on_play,
        &mut callbacks.on_play_done,
        &mut callbacks.on_done,
        &mut callbacks.on_update,
    ]
}

fn callbacks_mut(config: &mut ConfigDraft) -> impl Iterator<Item = &mut RtmpCallbackConfig> {
    config.rtmp_services.iter_mut().flat_map(|service| {
        std::iter::once(&mut service.callbacks).chain(
            service
                .applications
                .iter_mut()
                .map(|application| &mut application.callbacks),
        )
    })
}

fn callbacks(config: &ConfigDraft) -> impl Iterator<Item = &RtmpCallbackConfig> {
    config.rtmp_services.iter().flat_map(|service| {
        std::iter::once(&service.callbacks).chain(
            service
                .applications
                .iter()
                .map(|application| &application.callbacks),
        )
    })
}

pub(super) fn has_secret_callback_url(config: &ConfigDraft) -> bool {
    callbacks(config)
        .flat_map(urls)
        .flatten()
        .any(|url| redact_callback_url(url) != *url)
}

pub(super) fn redact_callback_urls(config: &mut ConfigDraft) {
    for url in callbacks_mut(config).flat_map(urls_mut).flatten() {
        *url = redact_callback_url(url);
    }
}

pub(crate) fn contains_redacted_callback_url(config: &ConfigDraft) -> bool {
    callbacks(config)
        .flat_map(urls)
        .flatten()
        .any(|url| url.contains(REDACTED_PLACEHOLDER))
}

pub(crate) fn restore_redacted_callback_urls(draft: &mut ConfigDraft, authoritative: &ConfigDraft) {
    for service in &mut draft.rtmp_services {
        let Some(authoritative_service) = authoritative
            .rtmp_services
            .iter()
            .find(|candidate| candidate.name == service.name)
        else {
            continue;
        };
        restore_callbacks(&mut service.callbacks, &authoritative_service.callbacks);
        for application in &mut service.applications {
            if let Some(authoritative_application) = authoritative_service
                .applications
                .iter()
                .find(|candidate| candidate.name == application.name)
            {
                restore_callbacks(
                    &mut application.callbacks,
                    &authoritative_application.callbacks,
                );
            }
        }
    }
}

fn restore_callbacks(draft: &mut RtmpCallbackConfig, authoritative: &RtmpCallbackConfig) {
    for (url, authoritative_url) in urls_mut(draft).into_iter().zip(urls(authoritative)) {
        if let (Some(url), Some(authoritative_url)) = (url, authoritative_url)
            && url.contains(REDACTED_PLACEHOLDER)
            && *url == redact_callback_url(authoritative_url)
        {
            url.clone_from(authoritative_url);
        }
    }
}
