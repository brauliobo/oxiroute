#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) const MAX_STATIC_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_AUTOINDEX_ENTRIES: usize = 10_000;
const MAX_AUTOINDEX_BYTES: usize = 4 * 1024 * 1024;

pub(crate) fn nginx_add_header_status(status: u16) -> bool {
    matches!(
        status,
        200 | 201 | 204 | 206 | 301 | 302 | 303 | 304 | 307 | 308
    )
}

#[derive(Clone, Debug)]
pub(crate) struct StaticFile {
    pub(crate) content: StaticContent,
    pub(crate) content_type: HeaderValue,
    pub(crate) etag: HeaderValue,
    pub(crate) content_length: HeaderValue,
    pub(crate) last_modified: HeaderValue,
    pub(crate) modified: std::time::SystemTime,
    pub(crate) name: Arc<OsStr>,
    pub(crate) size: u64,
}

impl StaticFile {
    /// Reads a small file into memory so that serving it needs no further file access.
    pub(super) fn load(self) -> Result<Self, StaticServeError> {
        Ok(Self {
            content: self.content.load(self.size)?,
            ..self
        })
    }
}

#[derive(Debug)]
pub(crate) enum StaticTarget {
    File(StaticFile),
    Autoindex { body: Bytes },
    DirectoryRedirect { path: String },
    InternalRedirect { path: String },
    Status(u16),
}

impl StaticTarget {
    pub(super) fn load(self) -> Result<Self, StaticServeError> {
        match self {
            Self::File(file) => file.load().map(Self::File),
            other => Ok(other),
        }
    }
}

#[derive(Debug)]
pub(crate) enum StaticErrorTarget {
    File {
        file: StaticFile,
        headers: Box<[(HeaderName, HeaderValue)]>,
    },
    InternalRedirect {
        path: String,
        headers: Box<[(HeaderName, HeaderValue)]>,
    },
    Literal {
        body: Bytes,
        headers: Box<[(HeaderName, HeaderValue)]>,
    },
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("HTTP static root failed secure preflight")]
pub(crate) struct StaticPreflightError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum StaticServeError {
    #[error("static target was not found")]
    NotFound,
    #[error("static target is not safely servable")]
    Unsafe,
    #[error("static target exceeds the serving bound")]
    TooLarge,
    #[error("static target could not be read")]
    Unavailable,
}

pub(super) fn open_pinned_directory(path: &Path) -> Result<OwnedFd, rustix::io::Errno> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut directory = rustix_fs::open(Path::new("/"), flags, Mode::empty())?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                directory = rustix_fs::openat(&directory, name, flags, Mode::empty())?;
            }
            Component::ParentDir | Component::Prefix(_) => return Err(rustix::io::Errno::INVAL),
        }
    }
    Ok(directory)
}

pub(super) fn read_static_target(
    root: &OwnedFd,
    components: &[OsString],
    policy: &StaticDirectoryPolicy,
    require_directory: bool,
    redirect_directory: bool,
    request_path: &str,
) -> Result<StaticTarget, StaticServeError> {
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut directory =
        rustix::io::fcntl_dupfd_cloexec(root, 0).map_err(|_| StaticServeError::Unavailable)?;
    let Some((file_name, parents)) = components.split_last() else {
        return read_directory(&directory, policy, false, request_path);
    };
    for parent in parents {
        directory = rustix_fs::openat(&directory, parent, directory_flags, Mode::empty())
            .map_err(static_open_error)?;
    }
    let descriptor = rustix_fs::openat(
        &directory,
        file_name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(static_open_error)?;
    let metadata = rustix_fs::fstat(&descriptor).map_err(|_| StaticServeError::Unavailable)?;
    if FileType::from_raw_mode(metadata.st_mode).is_dir() {
        return read_directory(&descriptor, policy, redirect_directory, request_path);
    }
    if require_directory {
        return Err(StaticServeError::NotFound);
    }
    read_regular_file(descriptor, file_name).map(StaticTarget::File)
}

fn read_directory(
    directory: &OwnedFd,
    policy: &StaticDirectoryPolicy,
    redirect: bool,
    request_path: &str,
) -> Result<StaticTarget, StaticServeError> {
    if redirect {
        return Ok(StaticTarget::DirectoryRedirect {
            path: format!("{request_path}/"),
        });
    }
    for index in &policy.indexes {
        let descriptor = match rustix_fs::openat(
            directory,
            index,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(Errno::NOENT) => continue,
            Err(error) => return Err(static_open_error(error)),
        };
        match read_regular_file(descriptor, index) {
            Err(StaticServeError::NotFound) => {}
            Ok(_file) if policy.internal_index_redirects => {
                let Some(index) = index.to_str() else {
                    return Err(StaticServeError::Unsafe);
                };
                return Ok(StaticTarget::InternalRedirect {
                    path: format!("{request_path}{index}"),
                });
            }
            result => return result.map(StaticTarget::File),
        }
    }
    if policy.autoindex {
        render_autoindex(directory, policy.exact_size, policy.local_time)
            .map(|body| StaticTarget::Autoindex { body })
    } else {
        Err(StaticServeError::Unsafe)
    }
}

fn static_open_error(error: Errno) -> StaticServeError {
    match error {
        Errno::NOENT => StaticServeError::NotFound,
        Errno::LOOP | Errno::NOTDIR | Errno::ACCESS | Errno::PERM => StaticServeError::Unsafe,
        _ => StaticServeError::Unavailable,
    }
}

fn read_regular_file(descriptor: OwnedFd, name: &OsStr) -> Result<StaticFile, StaticServeError> {
    let before = rustix_fs::fstat(&descriptor).map_err(|_| StaticServeError::Unavailable)?;
    if !FileType::from_raw_mode(before.st_mode).is_file() {
        return Err(StaticServeError::Unsafe);
    }
    let size = u64::try_from(before.st_size).map_err(|_| StaticServeError::TooLarge)?;
    if size > MAX_STATIC_FILE_BYTES {
        return Err(StaticServeError::TooLarge);
    }
    let modified = u64::try_from(before.st_mtime)
        .ok()
        .and_then(|seconds| {
            std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(seconds))
        })
        .unwrap_or(std::time::UNIX_EPOCH);
    Ok(StaticFile {
        content: StaticContent::open(Arc::new(File::from(descriptor))),
        content_type: HeaderValue::from_static(builtin_content_type(name)),
        etag: nginx_static_etag(before.st_mtime, size),
        content_length: HeaderValue::from(size),
        last_modified: HeaderValue::from_str(&httpdate::fmt_http_date(modified))
            .expect("HTTP date is a valid header value"),
        modified,
        name: name.into(),
        size,
    })
}

pub(super) fn nginx_static_etag(modified: i64, size: u64) -> HeaderValue {
    HeaderValue::from_str(&format!("\"{modified:x}-{size:x}\""))
        .expect("stat fields produce a valid ETag")
}

struct AutoindexEntry {
    directory: bool,
    modified: i64,
    name: Vec<u8>,
    size: u64,
}

fn render_autoindex(
    directory: &OwnedFd,
    exact_size: bool,
    local_time: bool,
) -> Result<Bytes, StaticServeError> {
    let mut entries = Vec::new();
    let mut reader = Dir::read_from(directory).map_err(|_| StaticServeError::Unavailable)?;
    for entry in &mut reader {
        let entry = entry.map_err(|_| StaticServeError::Unavailable)?;
        let name = entry.file_name().to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        if entries.len() >= MAX_AUTOINDEX_ENTRIES {
            return Err(StaticServeError::TooLarge);
        }
        let metadata =
            match rustix_fs::statat(directory, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(metadata) => metadata,
                Err(Errno::NOENT) => continue,
                Err(_) => return Err(StaticServeError::Unavailable),
            };
        let kind = FileType::from_raw_mode(metadata.st_mode);
        if !kind.is_file() && !kind.is_dir() {
            continue;
        }
        entries.push(AutoindexEntry {
            directory: kind.is_dir(),
            modified: metadata.st_mtime,
            name: name.to_vec(),
            size: u64::try_from(metadata.st_size).unwrap_or(0),
        });
    }
    entries.sort_unstable_by(|left, right| left.name.cmp(&right.name));

    let mut output = String::from(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Index</title></head><body><h1>Index</h1><pre><a href=\"../\">../</a>\n",
    );
    for entry in entries {
        let href = percent_encode_path_segment(&entry.name);
        let label_value = String::from_utf8_lossy(&entry.name);
        let mut label = String::with_capacity(label_value.len());
        crate::html::escape_html(&mut label, &label_value);
        let suffix = if entry.directory { "/" } else { "" };
        let modified = autoindex_time(entry.modified, local_time);
        let size = if entry.directory {
            "-".to_owned()
        } else if exact_size {
            entry.size.to_string()
        } else {
            human_size(entry.size)
        };
        writeln!(
            output,
            "<a href=\"{href}{suffix}\">{label}{suffix}</a>  {modified}  {size}"
        )
        .map_err(|_| StaticServeError::Unavailable)?;
        if output.len() > MAX_AUTOINDEX_BYTES {
            return Err(StaticServeError::TooLarge);
        }
    }
    output.push_str("</pre></body></html>\n");
    Ok(Bytes::from(output))
}

fn autoindex_time(timestamp: i64, local: bool) -> String {
    let Ok(mut value) = time::OffsetDateTime::from_unix_timestamp(timestamp) else {
        return "1970-01-01 00:00".into();
    };
    if local && let Ok(offset) = time::UtcOffset::local_offset_at(value) {
        value = value.to_offset(offset);
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        value.year(),
        u8::from(value.month()),
        value.day(),
        value.hour(),
        value.minute()
    )
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024 && unit < UNITS.len() - 1 {
        value = value.saturating_add(1023) / 1024;
        unit += 1;
    }
    format!("{value}{}", UNITS[unit])
}

fn percent_encode_path_segment(value: &[u8]) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(*byte));
        } else {
            write!(encoded, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
    encoded
}

fn same_file_snapshot(first: &rustix_fs::Stat, second: &rustix_fs::Stat) -> bool {
    first.st_dev == second.st_dev
        && first.st_ino == second.st_ino
        && first.st_mode == second.st_mode
        && first.st_size == second.st_size
        && first.st_mtime == second.st_mtime
        && first.st_mtime_nsec == second.st_mtime_nsec
        && first.st_ctime == second.st_ctime
        && first.st_ctime_nsec == second.st_ctime_nsec
}

pub(super) fn read_secret_file(
    path: &Path,
    max_bytes: usize,
) -> Result<Zeroizing<Vec<u8>>, AccessPreflightError> {
    let descriptor = rustix_fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|_| AccessPreflightError)?;
    let before = rustix_fs::fstat(&descriptor).map_err(|_| AccessPreflightError)?;
    if !FileType::from_raw_mode(before.st_mode).is_file()
        || !matches!(before.st_mode & 0o7777, 0o400 | 0o600 | 0o440 | 0o640)
    {
        return Err(AccessPreflightError);
    }
    let size = usize::try_from(before.st_size).map_err(|_| AccessPreflightError)?;
    if size > max_bytes {
        return Err(AccessPreflightError);
    }
    let mut file = File::from(descriptor);
    let mut bytes = Zeroizing::new(Vec::with_capacity(size));
    std::io::Read::by_ref(&mut file)
        .take(u64::try_from(max_bytes + 1).map_err(|_| AccessPreflightError)?)
        .read_to_end(&mut bytes)
        .map_err(|_| AccessPreflightError)?;
    let after = rustix_fs::fstat(&file).map_err(|_| AccessPreflightError)?;
    if bytes.len() > max_bytes || bytes.len() != size || !same_file_snapshot(&before, &after) {
        return Err(AccessPreflightError);
    }
    Ok(bytes)
}

pub(super) fn request_components(path: &str) -> Result<Vec<OsString>, StaticServeError> {
    path.trim_start_matches('/')
        .split('/')
        .filter(|component| !component.is_empty())
        .map(|component| {
            let decoded = percent_decode(component.as_bytes())?;
            if decoded.is_empty()
                || decoded.as_slice() == b"."
                || decoded.as_slice() == b".."
                || decoded.contains(&0)
                || decoded.contains(&b'/')
                || decoded.contains(&b'\\')
            {
                return Err(StaticServeError::Unsafe);
            }
            Ok(OsString::from_vec(decoded))
        })
        .collect()
}

pub(super) fn path_components(path: &Path) -> Result<Vec<OsString>, ()> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => components.push(value.to_os_string()),
            Component::CurDir if path.as_os_str().is_empty() => {}
            Component::RootDir
            | Component::CurDir
            | Component::ParentDir
            | Component::Prefix(_) => return Err(()),
        }
    }
    Ok(components)
}

fn percent_decode(value: &[u8]) -> Result<Vec<u8>, StaticServeError> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        if value[index] == b'%' {
            let digits = value
                .get(index + 1..index + 3)
                .ok_or(StaticServeError::Unsafe)?;
            decoded.push(
                hex(digits[0])
                    .and_then(|high| hex(digits[1]).map(|low| high << 4 | low))
                    .ok_or(StaticServeError::Unsafe)?,
            );
            index += 3;
        } else {
            decoded.push(value[index]);
            index += 1;
        }
    }
    Ok(decoded)
}

const fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn builtin_content_type(name: &OsStr) -> &'static str {
    let extension = Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    match extension.to_ascii_lowercase().as_str() {
        "css" => "text/css; charset=utf-8",
        "gif" => "image/gif",
        "htm" | "html" => "text/html; charset=utf-8",
        "ico" => "image/x-icon",
        "jpeg" | "jpg" => "image/jpeg",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "xml" => "application/xml",
        _ => "application/octet-stream",
    }
}
