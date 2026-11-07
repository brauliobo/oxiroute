#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn secure_directory(path: &Path) -> Result<(), RtmpAutoPushError> {
    if path.as_os_str().is_empty() {
        return Err(RtmpAutoPushError::TransportUnavailable);
    }
    if fs::symlink_metadata(path).is_err() {
        fs::create_dir_all(path).map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
        fs::set_permissions(path, fs::Permissions::from_mode(DIRECTORY_MODE))
            .map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
    if !metadata.file_type().is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(RtmpAutoPushError::TransportUnavailable);
    }
    Ok(())
}

pub(super) fn load_secret(path: &Path, create: bool) -> Result<Vec<u8>, RtmpAutoPushError> {
    let file = if create {
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)
        {
            Ok(mut file) => {
                let secret = generated_secret();
                file.write_all(&secret)
                    .map_err(|_| RtmpAutoPushError::CredentialsUnavailable)?;
                file.sync_data()
                    .map_err(|_| RtmpAutoPushError::CredentialsUnavailable)?;
                return Ok(secret.to_vec());
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => File::open(path),
            Err(_) => return Err(RtmpAutoPushError::CredentialsUnavailable),
        }
    } else {
        File::open(path)
    }
    .map_err(|_| RtmpAutoPushError::CredentialsUnavailable)?;
    let metadata = file
        .metadata()
        .map_err(|_| RtmpAutoPushError::CredentialsUnavailable)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(RtmpAutoPushError::CredentialsUnavailable);
    }
    let mut bytes = Vec::new();
    file.take((MAX_SECRET_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| RtmpAutoPushError::CredentialsUnavailable)?;
    if bytes.len() != SECRET_BYTES {
        return Err(RtmpAutoPushError::CredentialsUnavailable);
    }
    Ok(bytes)
}

fn generated_secret() -> [u8; SECRET_BYTES] {
    let mut secret = [0; SECRET_BYTES];
    secret[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    secret[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    secret
}

pub(super) fn endpoint_path(
    directory: &Path,
    service_hash: [u8; 4],
    worker_id: WorkerId,
) -> Result<PathBuf, RtmpAutoPushError> {
    let name = format!(
        "{SOCKET_PREFIX}{}-{}{SOCKET_SUFFIX}",
        hex(&service_hash),
        hex(&worker_id)
    );
    let path = directory.join(name);
    if path.as_os_str().to_str().is_none() || path.as_os_str().to_string_lossy().len() > 107 {
        return Err(RtmpAutoPushError::TransportUnavailable);
    }
    Ok(path)
}

pub(super) fn parse_endpoint(path: &Path, service_hash: [u8; 4]) -> Option<WorkerId> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_socket() || metadata.permissions().mode() & 0o077 != 0 {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    let prefix = format!("{SOCKET_PREFIX}{}-", hex(&service_hash));
    let worker = name.strip_prefix(&prefix)?.strip_suffix(SOCKET_SUFFIX)?;
    decode_hex(worker)
}

fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(result, "{byte:02x}");
    }
    result
}

fn decode_hex(value: &str) -> Option<WorkerId> {
    if value.len() != 32 {
        return None;
    }
    let mut bytes = [0; 16];
    for (index, slot) in bytes.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}
