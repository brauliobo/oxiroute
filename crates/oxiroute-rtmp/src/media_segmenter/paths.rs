#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn resolve_public_path(
    config: &HlsOutputConfig,
    prefix: &Path,
    object: &str,
) -> Result<(PathBuf, &'static str), MediaStoreError> {
    if object.is_empty() || object.len() > MAX_PUBLIC_OBJECT_BYTES || object.contains('%') {
        return Err(MediaStoreError::InvalidPath);
    }
    let components: Vec<_> = object.split('/').collect();
    if components
        .iter()
        .any(|component| safe_public_component(component).is_err())
    {
        return Err(MediaStoreError::InvalidPath);
    }
    let variants = config.variants();
    if object == "index.m3u8" {
        return Ok((
            if variants.len() > 1 {
                prefix.join("master.m3u8")
            } else {
                config
                    .variant_directory(prefix, &variants[0])
                    .join("index.m3u8")
            },
            "application/vnd.apple.mpegurl",
        ));
    }
    if object == "master.m3u8" && variants.len() > 1 {
        return Ok((prefix.join("master.m3u8"), "application/vnd.apple.mpegurl"));
    }
    for variant in &variants {
        let directory = config.variant_directory(prefix, variant);
        let variant_prefix = if directory == prefix {
            String::new()
        } else {
            format!("{}/", variant.name)
        };
        if let Some(relative) = object.strip_prefix(&variant_prefix) {
            if relative == "index.m3u8" {
                return Ok((directory.join(relative), "application/vnd.apple.mpegurl"));
            }
            if has_extension(relative, "ts") || has_extension(relative, "bin") {
                if let Some(key_prefix) = config.keys.as_ref().map(|keys| keys.url_prefix.as_str())
                    && let Some(key_name) = relative.strip_prefix(key_prefix)
                    && has_extension(key_name, "bin")
                {
                    return Ok((directory.join(key_name), content_type(key_name)));
                }
                if has_extension(relative, "bin") && config.keys.is_some() {
                    return Ok((directory.join(relative), content_type(relative)));
                }
                if has_extension(relative, "bin") {
                    return Err(MediaStoreError::NotFound);
                }
                return Ok((directory.join(relative), content_type(relative)));
            }
        }
    }
    Err(MediaStoreError::NotFound)
}

const MAX_PUBLIC_OBJECT_BYTES: usize = 1_024;

pub(super) fn resolve_dash_public_path(
    config: &DashOutputConfig,
    prefix: &Path,
    object: &str,
) -> Result<(PathBuf, &'static str), MediaStoreError> {
    if object.is_empty() || object.len() > MAX_PUBLIC_OBJECT_BYTES || object.contains('%') {
        return Err(MediaStoreError::InvalidPath);
    }
    let components: Vec<_> = object.split('/').collect();
    if components
        .iter()
        .any(|component| safe_public_component(component).is_err())
    {
        return Err(MediaStoreError::InvalidPath);
    }
    let directory = config.media_directory(prefix);
    let relative = if config.nested {
        object.strip_prefix("dash/").unwrap_or(object)
    } else {
        object
    };
    if relative == "manifest.mpd" || relative == "index.mpd" {
        return Ok((directory.join("manifest.mpd"), "application/dash+xml"));
    }
    if relative == "init.mp4" {
        return Ok((directory.join(relative), "video/mp4"));
    }
    if relative.starts_with("seg-") && has_extension(relative, "m4s") {
        let sequence = relative
            .strip_suffix(".m4s")
            .and_then(|value| value.rsplit('-').next())
            .and_then(|value| value.parse::<u64>().ok());
        if relative.split('/').count() == 1 && sequence.is_some() {
            return Ok((directory.join(relative), "video/iso.segment"));
        }
    }
    Err(MediaStoreError::NotFound)
}

fn safe_public_component(component: &str) -> Result<(), MediaStoreError> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.contains(['\\', '\0'])
        || component.chars().any(char::is_control)
    {
        Err(MediaStoreError::InvalidPath)
    } else {
        Ok(())
    }
}

fn content_type(path: &str) -> &'static str {
    if has_extension(path, "ts") {
        "video/mp2t"
    } else if has_extension(path, "mpd") {
        "application/dash+xml"
    } else if has_extension(path, "mp4") || has_extension(path, "m4s") {
        "video/mp4"
    } else {
        "application/octet-stream"
    }
}

fn has_extension(path: &str, extension: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
}
