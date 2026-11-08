#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CacheStore {
    Memory {
        name: String,
        #[serde(default = "default_cache_max_bytes")]
        max_bytes: u64,
        #[serde(default = "default_cache_max_entries")]
        max_entries: u64,
        #[serde(default = "default_cache_max_object_bytes")]
        max_object_bytes: u64,
        #[serde(default = "default_cache_max_header_bytes")]
        max_header_bytes: u64,
        #[serde(default = "default_cache_max_key_bytes")]
        max_key_bytes: u64,
        #[serde(default = "default_cache_max_tag_bytes")]
        max_tag_bytes: u64,
        #[serde(default = "default_cache_max_tags_per_object")]
        max_tags_per_object: u64,
        #[serde(default = "default_cache_max_in_flight_fills")]
        max_in_flight_fills: u64,
        #[serde(default = "default_cache_max_followers_per_fill")]
        max_followers_per_fill: u64,
    },
    Disk {
        name: String,
        root_directory: PathBuf,
        #[serde(default = "default_disk_cache_max_bytes")]
        max_bytes: u64,
        #[serde(default = "default_disk_cache_max_files")]
        max_files: u64,
        #[serde(default = "default_cache_max_object_bytes")]
        max_object_bytes: u64,
        #[serde(default = "default_cache_max_header_bytes")]
        max_header_bytes: u64,
        #[serde(default = "default_cache_max_key_bytes")]
        max_key_bytes: u64,
        #[serde(default = "default_cache_max_tag_bytes")]
        max_tag_bytes: u64,
        #[serde(default = "default_cache_max_tags_per_object")]
        max_tags_per_object: u64,
        #[serde(default = "default_cache_max_in_flight_fills")]
        max_in_flight_fills: u64,
        #[serde(default = "default_cache_max_followers_per_fill")]
        max_followers_per_fill: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheStoreKind {
    Memory,
    Disk,
}

#[derive(Clone, Copy, Debug)]
pub struct CacheStoreCommon<'a> {
    pub name: &'a str,
    pub max_bytes: u64,
    pub max_entries: u64,
    pub max_object_bytes: u64,
    pub max_header_bytes: u64,
    pub max_key_bytes: u64,
    pub max_tag_bytes: u64,
    pub max_tags_per_object: u64,
    pub max_in_flight_fills: u64,
    pub max_followers_per_fill: u64,
}

impl CacheStore {
    #[must_use]
    pub fn memory(name: impl Into<String>, max_bytes: u64) -> Self {
        Self::Memory {
            name: name.into(),
            max_bytes,
            max_entries: default_cache_max_entries(),
            max_object_bytes: default_cache_max_object_bytes().min(max_bytes),
            max_header_bytes: default_cache_max_header_bytes().min(max_bytes),
            max_key_bytes: default_cache_max_key_bytes(),
            max_tag_bytes: default_cache_max_tag_bytes(),
            max_tags_per_object: default_cache_max_tags_per_object(),
            max_in_flight_fills: default_cache_max_in_flight_fills(),
            max_followers_per_fill: default_cache_max_followers_per_fill(),
        }
    }

    #[must_use]
    pub fn disk(name: impl Into<String>, root_directory: PathBuf, max_bytes: u64) -> Self {
        Self::Disk {
            name: name.into(),
            root_directory,
            max_bytes,
            max_files: default_disk_cache_max_files(),
            max_object_bytes: default_cache_max_object_bytes().min(max_bytes),
            max_header_bytes: default_cache_max_header_bytes().min(max_bytes),
            max_key_bytes: default_cache_max_key_bytes(),
            max_tag_bytes: default_cache_max_tag_bytes(),
            max_tags_per_object: default_cache_max_tags_per_object(),
            max_in_flight_fills: default_cache_max_in_flight_fills(),
            max_followers_per_fill: default_cache_max_followers_per_fill(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> CacheStoreKind {
        match self {
            Self::Memory { .. } => CacheStoreKind::Memory,
            Self::Disk { .. } => CacheStoreKind::Disk,
        }
    }

    #[must_use]
    pub fn common(&self) -> CacheStoreCommon<'_> {
        match self {
            Self::Memory {
                name,
                max_bytes,
                max_entries,
                max_object_bytes,
                max_header_bytes,
                max_key_bytes,
                max_tag_bytes,
                max_tags_per_object,
                max_in_flight_fills,
                max_followers_per_fill,
            } => CacheStoreCommon {
                name,
                max_bytes: *max_bytes,
                max_entries: *max_entries,
                max_object_bytes: *max_object_bytes,
                max_header_bytes: *max_header_bytes,
                max_key_bytes: *max_key_bytes,
                max_tag_bytes: *max_tag_bytes,
                max_tags_per_object: *max_tags_per_object,
                max_in_flight_fills: *max_in_flight_fills,
                max_followers_per_fill: *max_followers_per_fill,
            },
            Self::Disk {
                name,
                max_bytes,
                max_files,
                max_object_bytes,
                max_header_bytes,
                max_key_bytes,
                max_tag_bytes,
                max_tags_per_object,
                max_in_flight_fills,
                max_followers_per_fill,
                ..
            } => CacheStoreCommon {
                name,
                max_bytes: *max_bytes,
                max_entries: *max_files,
                max_object_bytes: *max_object_bytes,
                max_header_bytes: *max_header_bytes,
                max_key_bytes: *max_key_bytes,
                max_tag_bytes: *max_tag_bytes,
                max_tags_per_object: *max_tags_per_object,
                max_in_flight_fills: *max_in_flight_fills,
                max_followers_per_fill: *max_followers_per_fill,
            },
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        self.common().name
    }

    #[must_use]
    pub fn root_directory(&self) -> Option<&Path> {
        match self {
            Self::Memory { .. } => None,
            Self::Disk { root_directory, .. } => Some(root_directory.as_path()),
        }
    }

    pub(crate) const fn root_directory_mut(&mut self) -> Option<&mut PathBuf> {
        match self {
            Self::Memory { .. } => None,
            Self::Disk { root_directory, .. } => Some(root_directory),
        }
    }
}
