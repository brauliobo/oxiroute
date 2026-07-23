use std::{collections::HashMap, fs, io, path::Path};

use super::ApiResponse;

struct StaticAsset {
    body: Vec<u8>,
    content_type: &'static str,
}

pub(super) struct UiAssets {
    index: StaticAsset,
    assets: HashMap<String, StaticAsset>,
}

impl UiAssets {
    pub(super) fn load(directory: &Path) -> io::Result<Self> {
        let index = StaticAsset {
            body: fs::read(directory.join("index.html"))?,
            content_type: "text/html; charset=utf-8",
        };
        let mut assets = HashMap::new();
        let assets_directory = directory.join("assets");
        if assets_directory.is_dir() {
            for entry in fs::read_dir(assets_directory)? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                assets.insert(
                    format!("/assets/{file_name}"),
                    StaticAsset {
                        body: fs::read(entry.path())?,
                        content_type: asset_content_type(&file_name),
                    },
                );
            }
        }
        Ok(Self { index, assets })
    }

    pub(super) fn response(&self, path: &str) -> Option<ApiResponse> {
        let asset = match path {
            "/" | "/index.html" => Some(&self.index),
            _ => self.assets.get(path),
        }?;
        Some(ApiResponse::bytes(
            200,
            asset.body.clone(),
            asset.content_type,
        ))
    }
}

fn asset_content_type(file_name: &str) -> &'static str {
    match Path::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}
