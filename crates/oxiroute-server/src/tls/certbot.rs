use std::{
    ffi::{OsStr, OsString},
    fmt,
    fs::{self, File},
    io::{self, Read},
    os::unix::{ffi::OsStringExt as _, fs::MetadataExt as _},
    path::{Component, Path, PathBuf},
};

use rustix::{
    fd::OwnedFd,
    fs::{self as rustix_fs, AtFlags, FileType, Mode, OFlags},
    io::Errno,
};
use zeroize::Zeroizing;

use super::{
    MAX_CERTIFICATE_CHAIN_BYTES, MAX_PRIVATE_KEY_BYTES, TlsBuildError,
    certificate::CertificateGeneration, pem_labels,
};

const CERTBOT_CERTIFICATE_FILE: &str = "Certbot certificate";
const CERTBOT_CHAIN_FILE: &str = "Certbot chain";
const CERTBOT_FULLCHAIN_FILE: &str = "Certbot full chain";
const CERTBOT_PRIVATE_KEY_FILE: &str = "Certbot private key";

struct PinnedArchive {
    path: PathBuf,
    descriptor: OwnedFd,
}

impl PinnedArchive {
    fn open(certificate: &str, path: &Path) -> Result<Self, TlsBuildError> {
        let path = canonical_directory(certificate, "archive", path)?;
        let expected = fs::metadata(&path).map_err(|source| TlsBuildError::FileMetadata {
            owner: certificate.into(),
            kind: "Certbot archive directory",
            path: path.clone(),
            source,
        })?;
        let descriptor = rustix_fs::open(
            &path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|source| TlsBuildError::CertbotDirectoryCanonicalization {
            certificate: certificate.into(),
            kind: "archive",
            path: path.clone(),
            source: source.into(),
        })?;
        let actual =
            rustix_fs::fstat(&descriptor).map_err(|source| TlsBuildError::FileMetadata {
                owner: certificate.into(),
                kind: "Certbot archive directory",
                path: path.clone(),
                source: source.into(),
            })?;
        if actual.st_dev != expected.dev() || actual.st_ino != expected.ino() {
            return Err(TlsBuildError::CertbotLineageChanged {
                certificate: certificate.into(),
            });
        }

        Ok(Self { path, descriptor })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn verify_path_identity(&self, certificate: &str) -> Result<(), TlsBuildError> {
        let current = rustix_fs::open(
            &self.path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .and_then(rustix_fs::fstat)
        .map_err(|_source| TlsBuildError::CertbotLineageChanged {
            certificate: certificate.into(),
        })?;
        let pinned = rustix_fs::fstat(&self.descriptor).map_err(|_source| {
            TlsBuildError::CertbotLineageChanged {
                certificate: certificate.into(),
            }
        })?;
        if current.st_dev != pinned.st_dev || current.st_ino != pinned.st_ino {
            return Err(TlsBuildError::CertbotLineageChanged {
                certificate: certificate.into(),
            });
        }
        Ok(())
    }

    fn display_path(&self, name: &OsStr) -> PathBuf {
        self.path.join(name)
    }

    fn read_bounded_stable(
        &self,
        owner: &str,
        kind: &'static str,
        name: &OsStr,
        limit: usize,
        private_key: bool,
    ) -> Result<Zeroizing<Vec<u8>>, TlsBuildError> {
        let first = self.read_bounded_once(owner, kind, name, limit, private_key)?;
        let second = self.read_bounded_once(owner, kind, name, limit, private_key)?;
        if first.as_slice() != second.as_slice() {
            return Err(TlsBuildError::FileChanged {
                owner: owner.into(),
                kind,
                path: self.display_path(name),
            });
        }
        Ok(first)
    }

    fn read_bounded_once(
        &self,
        owner: &str,
        kind: &'static str,
        name: &OsStr,
        limit: usize,
        private_key: bool,
    ) -> Result<Zeroizing<Vec<u8>>, TlsBuildError> {
        let path = self.display_path(name);
        if !is_archive_file_name(name) {
            return Err(TlsBuildError::CertbotArchiveEntryMetadata {
                certificate: owner.into(),
                kind,
                path,
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "archive entry name must be one path component",
                ),
            });
        }
        let descriptor = match rustix_fs::openat(
            &self.descriptor,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(Errno::LOOP) => {
                return Err(TlsBuildError::CertbotArchiveEntryNotRegular {
                    certificate: owner.into(),
                    kind,
                    path,
                });
            }
            Err(source) => {
                return Err(TlsBuildError::CertbotArchiveEntryMetadata {
                    certificate: owner.into(),
                    kind,
                    path,
                    source: source.into(),
                });
            }
        };
        let metadata = rustix_fs::fstat(&descriptor).map_err(|source| {
            TlsBuildError::CertbotArchiveEntryMetadata {
                certificate: owner.into(),
                kind,
                path: path.clone(),
                source: source.into(),
            }
        })?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file() {
            return Err(TlsBuildError::CertbotArchiveEntryNotRegular {
                certificate: owner.into(),
                kind,
                path,
            });
        }
        let size = usize::try_from(metadata.st_size).map_err(|_| TlsBuildError::FileTooLarge {
            owner: owner.into(),
            kind,
            path: path.clone(),
            limit,
        })?;
        if size > limit {
            return Err(TlsBuildError::FileTooLarge {
                owner: owner.into(),
                kind,
                path,
                limit,
            });
        }
        if private_key {
            let mode = metadata.st_mode & 0o7777;
            if !matches!(mode, 0o400 | 0o600 | 0o440 | 0o640) {
                return Err(TlsBuildError::InsecurePrivateKeyPermissions {
                    certificate: owner.into(),
                    path,
                });
            }
        }

        let mut file = File::from(descriptor);
        let mut bytes = Zeroizing::new(Vec::with_capacity(size));
        file.by_ref()
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|source| TlsBuildError::FileRead {
                owner: owner.into(),
                kind,
                path: path.clone(),
                source,
            })?;
        if bytes.len() > limit {
            return Err(TlsBuildError::FileTooLarge {
                owner: owner.into(),
                kind,
                path,
                limit,
            });
        }
        if bytes.is_empty() {
            return Err(TlsBuildError::EmptyFile {
                owner: owner.into(),
                kind,
                path,
            });
        }
        Ok(bytes)
    }

    fn file_type_no_follow(
        &self,
        certificate: &str,
        kind: &'static str,
        name: &OsStr,
    ) -> Result<FileType, TlsBuildError> {
        let path = self.display_path(name);
        if !is_archive_file_name(name) {
            return Err(TlsBuildError::CertbotArchiveEntryMetadata {
                certificate: certificate.into(),
                kind,
                path,
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "archive entry name must be one path component",
                ),
            });
        }
        let metadata = rustix_fs::statat(&self.descriptor, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|source| TlsBuildError::CertbotArchiveEntryMetadata {
                certificate: certificate.into(),
                kind,
                path,
                source: source.into(),
            })?;
        Ok(FileType::from_raw_mode(metadata.st_mode))
    }

    fn read_link(&self, certificate: &str, name: &OsStr) -> Result<PathBuf, TlsBuildError> {
        let path = self.display_path(name);
        if !is_archive_file_name(name) {
            return Err(TlsBuildError::InvalidCertbotArchivePrivateKeyLink {
                certificate: certificate.into(),
                path,
                target: name.into(),
            });
        }
        let target =
            rustix_fs::readlinkat(&self.descriptor, name, Vec::new()).map_err(|source| {
                TlsBuildError::CertbotArchivePrivateKeyLinkRead {
                    certificate: certificate.into(),
                    path,
                    source: source.into(),
                }
            })?;
        Ok(PathBuf::from(OsString::from_vec(target.into_bytes())))
    }
}

fn is_archive_file_name(name: &OsStr) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

/// One validated Certbot archive revision and its parsed immutable TLS generation.
pub struct CertbotCandidate {
    archive_revision: u64,
    generation: CertificateGeneration,
}

impl CertbotCandidate {
    #[must_use]
    pub const fn archive_revision(&self) -> u64 {
        self.archive_revision
    }

    #[must_use]
    pub const fn generation(&self) -> &CertificateGeneration {
        &self.generation
    }

    #[must_use]
    pub fn into_generation(self) -> CertificateGeneration {
        self.generation
    }
}

impl fmt::Debug for CertbotCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CertbotCandidate")
            .field("archive_revision", &self.archive_revision)
            .field("metadata", self.generation.metadata())
            .finish()
    }
}

/// An operator-owned Certbot live/archive lineage pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertbotLineage {
    live_directory_path: PathBuf,
    archive_directory_path: PathBuf,
}

impl CertbotLineage {
    #[must_use]
    pub fn new(
        live_directory_path: impl Into<PathBuf>,
        archive_directory_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            live_directory_path: live_directory_path.into(),
            archive_directory_path: archive_directory_path.into(),
        }
    }

    /// Loads one self-consistent Certbot lineage snapshot into an immutable TLS generation.
    ///
    /// # Errors
    ///
    /// Returns an error when the lineage directories, links, archive revision, PEM material, or
    /// resulting certificate identity fail validation.
    pub fn load(
        &self,
        name: impl Into<String>,
        declared_dns_names: &[String],
    ) -> Result<CertificateGeneration, TlsBuildError> {
        self.load_candidate(name, declared_dns_names)
            .map(CertbotCandidate::into_generation)
    }

    /// Loads one self-consistent Certbot archive revision and parsed generation.
    ///
    /// # Errors
    ///
    /// Returns an error when the lineage directories, links, archive revision, PEM material, or
    /// resulting certificate identity fail validation.
    pub fn load_candidate(
        &self,
        name: impl Into<String>,
        declared_dns_names: &[String],
    ) -> Result<CertbotCandidate, TlsBuildError> {
        let name = name.into();
        let snapshot = self.load_snapshot(&name)?;
        let generation = CertificateGeneration::from_pem(
            name,
            declared_dns_names,
            &snapshot.fullchain_path,
            &snapshot.fullchain_pem,
            &snapshot.private_key_path,
            &snapshot.private_key_pem,
        )?;
        Ok(CertbotCandidate {
            archive_revision: snapshot.archive_revision,
            generation,
        })
    }

    #[must_use]
    pub fn live_directory_path(&self) -> &Path {
        self.live_directory_path.as_path()
    }

    #[must_use]
    pub fn archive_directory_path(&self) -> &Path {
        self.archive_directory_path.as_path()
    }

    fn load_snapshot(&self, name: &str) -> Result<CertbotSnapshot, TlsBuildError> {
        let live_directory = canonical_directory(name, "live", &self.live_directory_path)?;
        let archive = PinnedArchive::open(name, &self.archive_directory_path)?;
        if live_directory == archive.path() {
            return Err(TlsBuildError::DuplicateCertbotDirectories {
                certificate: name.into(),
                path: live_directory,
            });
        }

        let initial_links = inspect_live_links(name, &live_directory, archive.path())?;
        let archive_revision = initial_links[0].revision;
        let cert_name = link_name(&initial_links, Artifact::Cert);
        let chain_name = link_name(&initial_links, Artifact::Chain);
        let fullchain_name = link_name(&initial_links, Artifact::Fullchain);
        let private_key_archive_name = link_name(&initial_links, Artifact::PrivateKey);

        let cert_pem = archive.read_bounded_stable(
            name,
            CERTBOT_CERTIFICATE_FILE,
            cert_name,
            MAX_CERTIFICATE_CHAIN_BYTES,
            false,
        )?;
        let chain_pem = archive.read_bounded_stable(
            name,
            CERTBOT_CHAIN_FILE,
            chain_name,
            MAX_CERTIFICATE_CHAIN_BYTES,
            false,
        )?;
        let cert_path = archive.display_path(cert_name);
        let chain_path = archive.display_path(chain_name);
        validate_certificate_artifact(name, &cert_path, &cert_pem)?;
        validate_chain_artifact(name, &chain_path, &chain_pem)?;
        let fullchain_pem = archive.read_bounded_stable(
            name,
            CERTBOT_FULLCHAIN_FILE,
            fullchain_name,
            MAX_CERTIFICATE_CHAIN_BYTES,
            false,
        )?;
        let fullchain_path = archive.display_path(fullchain_name);
        if fullchain_pem.len() != cert_pem.len() + chain_pem.len()
            || !fullchain_pem.starts_with(&cert_pem)
            || &fullchain_pem[cert_pem.len()..] != chain_pem.as_slice()
        {
            return Err(TlsBuildError::CertbotFullchainMismatch {
                certificate: name.into(),
                path: fullchain_path,
            });
        }

        let initial_private_key =
            resolve_archive_private_key(name, &archive, private_key_archive_name)?;
        let private_key_pem = archive.read_bounded_stable(
            name,
            CERTBOT_PRIVATE_KEY_FILE,
            initial_private_key.read_name(),
            MAX_PRIVATE_KEY_BYTES,
            true,
        )?;
        let final_private_key =
            resolve_archive_private_key(name, &archive, private_key_archive_name)?;
        if initial_private_key != final_private_key {
            return Err(TlsBuildError::CertbotLineageChanged {
                certificate: name.into(),
            });
        }

        let final_links = inspect_live_links(name, &live_directory, archive.path())?;
        if initial_links != final_links {
            return Err(TlsBuildError::CertbotLineageChanged {
                certificate: name.into(),
            });
        }
        archive.verify_path_identity(name)?;

        Ok(CertbotSnapshot {
            archive_revision,
            fullchain_path: archive.display_path(fullchain_name),
            fullchain_pem,
            private_key_path: archive.display_path(initial_private_key.read_name()),
            private_key_pem,
        })
    }
}

struct CertbotSnapshot {
    archive_revision: u64,
    fullchain_path: PathBuf,
    fullchain_pem: Zeroizing<Vec<u8>>,
    private_key_path: PathBuf,
    private_key_pem: Zeroizing<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Artifact {
    Cert,
    Chain,
    Fullchain,
    PrivateKey,
}

impl Artifact {
    const ALL: [Self; 4] = [Self::Cert, Self::Chain, Self::Fullchain, Self::PrivateKey];

    const fn live_name(self) -> &'static str {
        match self {
            Self::Cert => "cert.pem",
            Self::Chain => "chain.pem",
            Self::Fullchain => "fullchain.pem",
            Self::PrivateKey => "privkey.pem",
        }
    }

    const fn archive_stem(self) -> &'static str {
        match self {
            Self::Cert => "cert",
            Self::Chain => "chain",
            Self::Fullchain => "fullchain",
            Self::PrivateKey => "privkey",
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct LiveLink {
    artifact: Artifact,
    archive_name: OsString,
    revision: u64,
}

fn canonical_directory(
    certificate: &str,
    kind: &'static str,
    path: &Path,
) -> Result<PathBuf, TlsBuildError> {
    let canonical = fs::canonicalize(path).map_err(|source| {
        TlsBuildError::CertbotDirectoryCanonicalization {
            certificate: certificate.into(),
            kind,
            path: path.into(),
            source,
        }
    })?;
    let metadata = fs::metadata(&canonical).map_err(|source| TlsBuildError::FileMetadata {
        owner: certificate.into(),
        kind: "Certbot lineage directory",
        path: canonical.clone(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(TlsBuildError::CertbotPathNotDirectory {
            certificate: certificate.into(),
            kind,
            path: canonical,
        });
    }
    Ok(canonical)
}

fn inspect_live_links(
    certificate: &str,
    live_directory: &Path,
    archive_directory: &Path,
) -> Result<Vec<LiveLink>, TlsBuildError> {
    let mut links = Vec::with_capacity(Artifact::ALL.len());
    for artifact in Artifact::ALL {
        let path = live_directory.join(artifact.live_name());
        let metadata = fs::symlink_metadata(&path).map_err(|source| {
            TlsBuildError::CertbotLiveLinkMetadata {
                certificate: certificate.into(),
                path: path.clone(),
                source,
            }
        })?;
        if !metadata.file_type().is_symlink() {
            return Err(TlsBuildError::CertbotLiveEntryNotSymlink {
                certificate: certificate.into(),
                path,
            });
        }
        let target = fs::read_link(&path).map_err(|source| TlsBuildError::CertbotLiveLinkRead {
            certificate: certificate.into(),
            path: path.clone(),
            source,
        })?;
        let (archive_name, revision) =
            resolve_archive_artifact(live_directory, &target, archive_directory, artifact)
                .ok_or_else(|| TlsBuildError::InvalidCertbotLiveLinkTarget {
                    certificate: certificate.into(),
                    path,
                    target,
                })?;
        links.push(LiveLink {
            artifact,
            archive_name,
            revision,
        });
    }

    let revision = links[0].revision;
    if let Some(mixed) = links.iter().find(|link| link.revision != revision) {
        return Err(TlsBuildError::MixedCertbotArchiveRevisions {
            certificate: certificate.into(),
            expected: revision,
            found: mixed.revision,
        });
    }
    Ok(links)
}

fn link_name(links: &[LiveLink], artifact: Artifact) -> &OsStr {
    &links
        .iter()
        .find(|link| link.artifact == artifact)
        .expect("every Certbot artifact was inspected")
        .archive_name
}

fn numbered_revision(file_name: &std::ffi::OsStr, stem: &str) -> Option<u64> {
    let file_name = file_name.to_str()?;
    let revision = file_name.strip_prefix(stem)?.strip_suffix(".pem")?;
    if revision.starts_with('0') || !revision.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let parsed = revision.parse::<u64>().ok()?;
    (parsed > 0 && revision == parsed.to_string()).then_some(parsed)
}

fn normalize_link_target(parent: &Path, target: &Path) -> Option<PathBuf> {
    let joined = if target.is_absolute() {
        target.into()
    } else {
        parent.join(target)
    };
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }
    normalized.is_absolute().then_some(normalized)
}

fn resolve_archive_artifact(
    link_parent: &Path,
    target: &Path,
    archive_directory: &Path,
    artifact: Artifact,
) -> Option<(OsString, u64)> {
    let candidate = normalize_link_target(link_parent, target)?;
    let file_name = candidate.file_name()?;
    let revision = numbered_revision(file_name, artifact.archive_stem())?;
    let canonical_parent = fs::canonicalize(candidate.parent()?).ok()?;
    if canonical_parent != archive_directory {
        return None;
    }

    Some((file_name.to_owned(), revision))
}

fn validate_certificate_artifact(
    certificate: &str,
    path: &Path,
    pem: &[u8],
) -> Result<(), TlsBuildError> {
    let labels = pem_labels(certificate, CERTBOT_CERTIFICATE_FILE, path, pem)?;
    if labels.as_slice() != ["CERTIFICATE"] {
        return Err(TlsBuildError::InvalidPem {
            owner: certificate.into(),
            kind: CERTBOT_CERTIFICATE_FILE,
            path: path.into(),
            detail: "cert.pem must contain exactly one CERTIFICATE block",
        });
    }
    Ok(())
}

fn validate_chain_artifact(
    certificate: &str,
    path: &Path,
    pem: &[u8],
) -> Result<(), TlsBuildError> {
    let labels = pem_labels(certificate, CERTBOT_CHAIN_FILE, path, pem)?;
    if labels.iter().any(|label| *label != "CERTIFICATE") {
        return Err(TlsBuildError::InvalidPem {
            owner: certificate.into(),
            kind: CERTBOT_CHAIN_FILE,
            path: path.into(),
            detail: "chain.pem must contain one or more CERTIFICATE blocks only",
        });
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
enum ArchivePrivateKey {
    Regular(OsString),
    Reused { link: OsString, target: OsString },
}

impl ArchivePrivateKey {
    fn read_name(&self) -> &OsStr {
        match self {
            Self::Regular(name) => name,
            Self::Reused { target, .. } => target,
        }
    }
}

fn resolve_archive_private_key(
    certificate: &str,
    archive: &PinnedArchive,
    name: &OsStr,
) -> Result<ArchivePrivateKey, TlsBuildError> {
    let file_type = archive.file_type_no_follow(certificate, CERTBOT_PRIVATE_KEY_FILE, name)?;
    if file_type.is_file() {
        return Ok(ArchivePrivateKey::Regular(name.into()));
    }
    if !file_type.is_symlink() {
        return Err(TlsBuildError::CertbotArchiveEntryNotRegular {
            certificate: certificate.into(),
            kind: CERTBOT_PRIVATE_KEY_FILE,
            path: archive.display_path(name),
        });
    }

    let raw_target = archive.read_link(certificate, name)?;
    let target = resolve_archive_artifact(
        archive.path(),
        &raw_target,
        archive.path(),
        Artifact::PrivateKey,
    )
    .map(|(target, _revision)| target)
    .ok_or_else(|| TlsBuildError::InvalidCertbotArchivePrivateKeyLink {
        certificate: certificate.into(),
        path: archive.display_path(name),
        target: raw_target.clone(),
    })?;
    if !archive
        .file_type_no_follow(certificate, CERTBOT_PRIVATE_KEY_FILE, &target)?
        .is_file()
    {
        return Err(TlsBuildError::InvalidCertbotArchivePrivateKeyLink {
            certificate: certificate.into(),
            path: archive.display_path(name),
            target: raw_target,
        });
    }

    Ok(ArchivePrivateKey::Reused {
        link: name.into(),
        target,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use std::{ffi::OsStr, os::unix::fs::symlink};

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn fresh_descriptor_relative_nofollow_open_rejects_a_swap_to_an_outside_symlink() {
        let temp = TempDir::new().unwrap();
        let archive_path = temp.path().join("archive");
        fs::create_dir(&archive_path).unwrap();
        let cert_path = archive_path.join("cert1.pem");
        fs::write(&cert_path, b"trusted").unwrap();
        let archive = PinnedArchive::open("test", &archive_path).unwrap();
        let first = archive
            .read_bounded_once(
                "test",
                CERTBOT_CERTIFICATE_FILE,
                OsStr::new("cert1.pem"),
                1024,
                false,
            )
            .unwrap();
        assert_eq!(first.as_slice(), b"trusted");

        let outside = temp.path().join("outside.pem");
        fs::write(&outside, b"outside").unwrap();
        fs::remove_file(&cert_path).unwrap();
        symlink(&outside, &cert_path).unwrap();

        let error = archive
            .read_bounded_once(
                "test",
                CERTBOT_CERTIFICATE_FILE,
                OsStr::new("cert1.pem"),
                1024,
                false,
            )
            .unwrap_err();

        assert!(matches!(
            error,
            TlsBuildError::CertbotArchiveEntryNotRegular { .. }
        ));
    }

    #[test]
    fn pinned_archive_descriptor_detects_ancestor_replacement_after_reads() {
        let temp = TempDir::new().unwrap();
        let trusted_ancestor = temp.path().join("trusted");
        let archive_path = trusted_ancestor.join("archive");
        fs::create_dir_all(&archive_path).unwrap();
        fs::write(archive_path.join("cert1.pem"), b"trusted").unwrap();
        let archive = PinnedArchive::open("test", &archive_path).unwrap();

        let relocated_ancestor = temp.path().join("trusted-relocated");
        fs::rename(&trusted_ancestor, &relocated_ancestor).unwrap();
        let outside_ancestor = temp.path().join("outside");
        fs::create_dir_all(outside_ancestor.join("archive")).unwrap();
        fs::write(outside_ancestor.join("archive/cert1.pem"), b"outside").unwrap();
        symlink(&outside_ancestor, &trusted_ancestor).unwrap();

        let bytes = archive
            .read_bounded_stable(
                "test",
                CERTBOT_CERTIFICATE_FILE,
                OsStr::new("cert1.pem"),
                1024,
                false,
            )
            .unwrap();

        assert_eq!(bytes.as_slice(), b"trusted");
        assert!(matches!(
            archive.verify_path_identity("test"),
            Err(TlsBuildError::CertbotLineageChanged { .. })
        ));
    }
}
