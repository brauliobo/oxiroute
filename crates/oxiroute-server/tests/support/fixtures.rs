#![allow(dead_code)]

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

use tempfile::TempDir;

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn write_file_with_mode(directory: &Path, name: &str, contents: &[u8], mode: u32) -> PathBuf {
    let path = directory.join(name);
    fs::write(&path, contents).expect("write test secret");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("set test secret mode");
    path
}

pub fn write_secure_token(directory: &Path, name: &str, token: &str) -> PathBuf {
    write_file_with_mode(directory, name, format!("{token}\n").as_bytes(), 0o600)
}

pub fn copy_private_key_fixture(directory: &Path, name: &str) -> PathBuf {
    write_file_with_mode(
        directory,
        name,
        &fs::read(fixture(name)).expect("read private-key fixture"),
        0o600,
    )
}

pub struct PrivateKeyFixture {
    path: PathBuf,
    _directory: TempDir,
}

impl PrivateKeyFixture {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn private_key_fixture(name: &str) -> PrivateKeyFixture {
    let directory = TempDir::new().expect("private-key fixture directory");
    let path = copy_private_key_fixture(directory.path(), name);
    PrivateKeyFixture {
        path,
        _directory: directory,
    }
}

pub struct CertificateChainFixture {
    path: PathBuf,
    _directory: TempDir,
}

impl CertificateChainFixture {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn certificate_chain_fixture(leaf: &str, issuer: &str) -> CertificateChainFixture {
    let directory = TempDir::new().expect("certificate-chain fixture directory");
    let path = directory.path().join("fullchain.pem");
    let mut fullchain = fs::read(fixture(leaf)).expect("read leaf certificate fixture");
    fullchain.extend_from_slice(&fs::read(fixture(issuer)).expect("read issuer fixture"));
    fs::write(&path, fullchain).expect("write certificate-chain fixture");
    CertificateChainFixture {
        path,
        _directory: directory,
    }
}

pub fn write_test_identity(directory: &Path, private_key_name: &str) -> (PathBuf, PathBuf) {
    let certificate_path = directory.join("test-only-certificate-fullchain.pem");
    fs::copy(fixture("proxy-a.pem"), &certificate_path).expect("write test certificate fullchain");
    let private_key_path = write_file_with_mode(
        directory,
        private_key_name,
        &fs::read(fixture("proxy-a-key.pem")).expect("read test private-key fixture"),
        0o600,
    );
    (certificate_path, private_key_path)
}

pub fn create_secure_root(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir(&path).expect("create secure test root");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
        .expect("secure test root permissions");
    path
}
