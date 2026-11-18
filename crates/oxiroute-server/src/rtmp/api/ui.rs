use std::{collections::HashMap, fs, io, path::Path};

use http::{HeaderMap, header::IF_NONE_MATCH};
use openssl::sha::sha256;
use oxiroute_compress::{AcceptEncoding, Coding, Levels, compress};

use super::{
    ApiResponse,
    response::{HARDENED_HEADERS, matches_validator},
};
use crate::encoding::lower_hex;

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const REVALIDATE: &str = "no-cache";
const MIN_COMPRESSED_BYTES: usize = 256;
const PRECOMPRESSED: [Coding; 2] = [Coding::Brotli, Coding::Gzip];
const DIGEST_BYTES: usize = 16;

struct Variant {
    coding: Option<Coding>,
    etag: String,
    body: Vec<u8>,
}

struct StaticAsset {
    content_type: &'static str,
    cache_control: &'static str,
    identity: Variant,
    encoded: Vec<Variant>,
}

pub(crate) struct UiAssets {
    index: StaticAsset,
    assets: HashMap<String, StaticAsset>,
}

impl Variant {
    fn new(coding: Option<Coding>, digest: &str, body: Vec<u8>) -> Self {
        let suffix = coding.map_or(String::new(), |coding| format!("-{}", coding.token()));
        Self {
            coding,
            etag: format!("\"{digest}{suffix}\""),
            body,
        }
    }
}

impl StaticAsset {
    fn new(
        body: Vec<u8>,
        content_type: &'static str,
        cache_control: &'static str,
    ) -> io::Result<Self> {
        let digest = lower_hex(&sha256(&body)[..DIGEST_BYTES]);
        let mut encoded = Vec::new();
        if is_compressible(content_type) && body.len() >= MIN_COMPRESSED_BYTES {
            for coding in PRECOMPRESSED {
                let compressed =
                    compress(coding, Levels::default(), &body).map_err(io::Error::other)?;
                if compressed.len() < body.len() {
                    encoded.push(Variant::new(Some(coding), &digest, compressed));
                }
            }
        }
        Ok(Self {
            content_type,
            cache_control,
            identity: Variant::new(None, &digest, body),
            encoded,
        })
    }

    fn variant(&self, accepted: &AcceptEncoding) -> &Variant {
        let offered = self
            .encoded
            .iter()
            .filter_map(|variant| variant.coding)
            .collect::<Vec<_>>();
        accepted
            .select(&offered)
            .and_then(|coding| {
                self.encoded
                    .iter()
                    .find(|variant| variant.coding == Some(coding))
            })
            .unwrap_or(&self.identity)
    }

    fn response(&self, request: &HeaderMap) -> ApiResponse {
        let variant = self.variant(&AcceptEncoding::from_headers(request));
        let revalidated = request
            .get(IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| matches_validator(value, &variant.etag));
        let response = if revalidated {
            ApiResponse::not_modified(variant.etag.clone())
        } else {
            ApiResponse::bytes(200, variant.body.clone(), self.content_type)
                .with_etag(variant.etag.clone())
        };
        let response = match variant.coding.filter(|_| !revalidated) {
            Some(coding) => response.with_headers(&[("content-encoding", coding.token())]),
            None => response,
        };
        response
            .with_cache_control(self.cache_control)
            .with_headers(&[("vary", "accept-encoding")])
            .with_headers(&HARDENED_HEADERS)
    }
}

impl UiAssets {
    pub(crate) fn load(directory: &Path) -> io::Result<Self> {
        let index = StaticAsset::new(
            fs::read(directory.join("index.html"))?,
            "text/html; charset=utf-8",
            REVALIDATE,
        )?;
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
                    StaticAsset::new(
                        fs::read(entry.path())?,
                        asset_content_type(&file_name),
                        IMMUTABLE,
                    )?,
                );
            }
        }
        Ok(Self { index, assets })
    }

    pub(super) fn response(&self, path: &str, request: &HeaderMap) -> Option<ApiResponse> {
        let asset = match path {
            "/" | "/index.html" => Some(&self.index),
            _ => self.assets.get(path),
        }?;
        Some(asset.response(request))
    }
}

fn is_compressible(content_type: &str) -> bool {
    content_type.starts_with("text/") || content_type.starts_with("image/svg+xml")
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
