use std::{collections::HashMap, path::PathBuf};

use crate::apache::EffectiveVirtualHost;
use crate::{OperationalOverlayKind, OperationalOverlayRequirement, ProvenanceRole};
use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, TlsPolicy, TlsProfile, TlsVersion,
};

use super::{ApacheProvenance, Lowerer};

impl Lowerer {
    #[allow(clippy::too_many_lines)]
    pub(super) fn lower_tls(
        &mut self,
        virtual_hosts: &[EffectiveVirtualHost],
        listener_index: usize,
    ) -> (Vec<Certificate>, TlsProfile, Vec<ApacheProvenance>) {
        let mut certificates = Vec::<Certificate>::new();
        let mut names = Vec::<String>::new();
        let mut origins = Vec::new();
        let mut certificate_identities = HashMap::<(PathBuf, PathBuf), String>::new();
        for virtual_host in virtual_hosts {
            if !virtual_host.tls.engine_on || !virtual_host.blocked.is_empty() {
                continue;
            }
            let (Some(chain), Some(key)) = (
                virtual_host.tls.certificate_chain.as_ref(),
                virtual_host.tls.private_key.as_ref(),
            ) else {
                continue;
            };
            if let Some(engine_origin) = &virtual_host.tls.engine_origin {
                origins.push(self.origin(
                    engine_origin,
                    if virtual_host.tls.engine_inherited {
                        ProvenanceRole::Inherited
                    } else {
                        ProvenanceRole::Value
                    },
                ));
            }
            let chain_origin = self.origin(
                &chain.origin,
                if chain.inherited {
                    ProvenanceRole::Inherited
                } else {
                    ProvenanceRole::Value
                },
            );
            let key_origin = self.origin(
                &key.origin,
                if key.inherited {
                    ProvenanceRole::Inherited
                } else {
                    ProvenanceRole::Value
                },
            );
            origins.extend([chain_origin.clone(), key_origin.clone()]);
            for (kind, path, origin) in [
                ("certificate", &chain.path, chain_origin.clone()),
                ("private-key", &key.path, key_origin.clone()),
            ] {
                self.operational_overlays
                    .push(OperationalOverlayRequirement {
                        id: format!("apache-tls-{kind}-{}", origin.span.range().start()),
                        kind: OperationalOverlayKind::CertificateMaterial,
                        origin: Some(origin.clone()),
                        redacted_evidence: false,
                        values: vec![path.display().to_string()],
                        satisfied: true,
                    });
            }
            let identity = (chain.path.clone(), key.path.clone());
            let certificate_name = if let Some(name) = certificate_identities.get(&identity) {
                name.clone()
            } else {
                let name = format!("apache-certificate-{}", certificates.len() + 1);
                certificate_identities.insert(identity, name.clone());
                certificates.push(Certificate {
                    name: name.clone(),
                    dns_names: Vec::new(),
                    source: CertificateSource::Files {
                        certificate_chain_path: chain.path.clone(),
                        private_key_path: key.path.clone(),
                    },
                });
                name
            };
            if !names.contains(&certificate_name) {
                names.push(certificate_name.clone());
            }
            let certificate = certificates
                .iter_mut()
                .find(|certificate| certificate.name == certificate_name)
                .expect("certificate identity was inserted");
            for name in &virtual_host.names {
                if !certificate.dns_names.contains(&name.certificate_name) {
                    certificate.dns_names.push(name.certificate_name.clone());
                }
                origins.push(self.origin(
                    &name.origin,
                    if name.inherited {
                        ProvenanceRole::Inherited
                    } else {
                        ProvenanceRole::Value
                    },
                ));
            }
        }
        let Some(default_certificate) = names.first().cloned() else {
            return (
                Vec::new(),
                TlsProfile {
                    name: format!("apache-tls-profile-{}", listener_index + 1),
                    certificates: vec!["missing".into()],
                    default_certificate: "missing".into(),
                    min_version: TlsVersion::Tls12,
                    alpn: vec![AlpnProtocol::Http11],
                    policy: TlsPolicy::default(),
                },
                origins,
            );
        };
        let profile = TlsProfile {
            name: format!("apache-tls-profile-{}", listener_index + 1),
            certificates: names,
            default_certificate,
            min_version: TlsVersion::Tls12,
            alpn: vec![AlpnProtocol::Http11],
            policy: TlsPolicy::default(),
        };
        (certificates, profile, origins)
    }

    pub(super) fn commit_certificates(
        &mut self,
        certificates: Vec<Certificate>,
        origins: &[ApacheProvenance],
    ) {
        for certificate in certificates {
            let index = self.draft.certificates.len();
            let path = format!("/certificates/{index}");
            self.draft.certificates.push(certificate);
            for suffix in [
                "",
                "/name",
                "/dns_names",
                "/source",
                "/source/type",
                "/source/certificate_chain_path",
                "/source/private_key_path",
            ] {
                self.record(format!("{path}{suffix}"), origins.to_vec());
            }
        }
    }
}
