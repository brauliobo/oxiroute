#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HlsFragmentNaming {
    Sequential,
    Timestamp,
    System,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HlsVariant {
    pub name: String,
    pub bandwidth: u64,
    pub codecs: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HlsKeyConfig {
    pub rotation_segments: usize,
    pub url_prefix: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HlsValueError {
    #[error("HLS field `{0}` is invalid")]
    InvalidField(&'static str),
}

impl HlsVariant {
    /// # Errors
    ///
    /// Returns an error for an invalid variant identity, media bound, or codecs value.
    pub fn validate_intrinsic(&self) -> Result<(), HlsValueError> {
        if self.name.is_empty()
            || self.name.len() > MAX_HLS_VARIANT_NAME_BYTES
            || !self
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(HlsValueError::InvalidField("variant.name"));
        }
        if self.bandwidth == 0
            || self.bandwidth > 9_007_199_254_740_991
            || self.width.is_some() != self.height.is_some()
            || self.width == Some(0)
            || self.height == Some(0)
        {
            return Err(HlsValueError::InvalidField("variant.bounds"));
        }
        if self.codecs.as_ref().is_some_and(|value| {
            value.is_empty()
                || value.len() > 128
                || !value.is_ascii()
                || value
                    .bytes()
                    .any(|byte| byte.is_ascii_control() || matches!(byte, b'"' | b'\\'))
        }) {
            return Err(HlsValueError::InvalidField("variant.codecs"));
        }
        Ok(())
    }
}

impl HlsKeyConfig {
    /// # Errors
    ///
    /// Returns an error for a zero rotation interval or invalid URL prefix.
    pub fn validate_intrinsic(&self) -> Result<(), HlsValueError> {
        if self.rotation_segments == 0 {
            return Err(HlsValueError::InvalidField("keys.rotation_segments"));
        }
        if self.url_prefix.len() > MAX_HLS_KEY_URL_PREFIX_BYTES
            || !self.url_prefix.is_ascii()
            || self.url_prefix.bytes().any(|byte| {
                byte.is_ascii_control() || matches!(byte, b'?' | b'#' | b'\\' | b'"' | b'%' | b' ')
            })
            || !self.url_prefix.is_empty()
                && (!self.url_prefix.ends_with('/')
                    || self.url_prefix.starts_with('/')
                    || self.url_prefix[..self.url_prefix.len() - 1]
                        .split('/')
                        .any(|component| component.is_empty() || matches!(component, "." | "..")))
        {
            return Err(HlsValueError::InvalidField("keys.url_prefix"));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct HlsOutputConfig {
    pub store: Arc<MediaStore>,
    pub segment_duration: Duration,
    pub max_segment_duration: Duration,
    pub playlist_length: Duration,
    pub naming: HlsFragmentNaming,
    pub nested: bool,
    pub cleanup: bool,
    pub variants: Vec<HlsVariant>,
    pub keys: Option<HlsKeyConfig>,
    pub max_segment_bytes: usize,
    pub max_queue_messages: usize,
}

impl HlsOutputConfig {
    pub(super) fn segment_window(&self) -> SegmentWindowConfig {
        SegmentWindowConfig::new(self.segment_duration, self.max_segment_duration)
    }

    pub(super) fn variants(&self) -> Vec<HlsVariant> {
        if self.variants.is_empty() {
            return vec![HlsVariant {
                name: "main".into(),
                bandwidth: 1_000_000,
                codecs: Some("avc1.42e01e,mp4a.40.2".into()),
                width: None,
                height: None,
            }];
        }
        self.variants.clone()
    }

    pub(super) fn variant_directory(&self, prefix: &Path, variant: &HlsVariant) -> PathBuf {
        let variants = self.variants();
        if self.nested || variants.len() > 1 {
            prefix.join(&variant.name)
        } else {
            prefix.to_path_buf()
        }
    }
}
