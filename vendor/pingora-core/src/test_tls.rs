// Copyright 2025 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! A throwaway self-signed certificate for the tests that need a TLS server, generated per test so no key is
//! stored in the repository.

use openssl::asn1::Asn1Time;
use openssl::ec::{EcGroup, EcKey};
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::PKey;
use openssl::x509::{X509Builder, X509NameBuilder};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// PEM files of a self-signed certificate for `localhost`, removed when dropped.
pub(crate) struct TestCert {
    pub cert_path: String,
    pub key_path: String,
    dir: PathBuf,
}

impl TestCert {
    pub fn new() -> Self {
        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
        let key = PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();

        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_nid(Nid::COMMONNAME, "localhost")
            .unwrap();
        let name = name.build();

        let mut cert = X509Builder::new().unwrap();
        cert.set_version(2).unwrap();
        cert.set_subject_name(&name).unwrap();
        cert.set_issuer_name(&name).unwrap();
        cert.set_pubkey(&key).unwrap();
        cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
            .unwrap();
        cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
            .unwrap();
        cert.sign(&key, MessageDigest::sha256()).unwrap();

        let dir = std::env::temp_dir().join(format!(
            "pingora-test-cert-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cert_path = dir.join("server.crt");
        let key_path = dir.join("key.pem");
        std::fs::write(&cert_path, cert.build().to_pem().unwrap()).unwrap();
        std::fs::write(&key_path, key.private_key_to_pem_pkcs8().unwrap()).unwrap();
        Self {
            cert_path: cert_path.to_str().unwrap().into(),
            key_path: key_path.to_str().unwrap().into(),
            dir,
        }
    }
}

impl Drop for TestCert {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
