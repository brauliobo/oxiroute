use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, Protocol, TlsProfile, TlsVersion,
};

use super::Lowerer;
use crate::haproxy::{BindTls, EffectiveValue, TlsAlpn, TlsMinimumVersion};

use super::provenance::{CanonicalPath, provenance_sources};

impl Lowerer<'_> {
    pub(super) fn lower_bind_tls(
        &mut self,
        tls: &EffectiveValue<BindTls>,
        protocol: Protocol,
        listener_index: usize,
    ) -> Option<String> {
        if protocol != Protocol::Http {
            self.block_value(
                tls,
                "HAProxy TLS termination on a non-HTTP listener has no canonical representation",
            );
            return None;
        }
        if tls.value.dns_names.is_empty() {
            self.block_value(
                tls,
                "HAProxy TLS listener has no exact certificate identities and cannot be lowered",
            );
            return None;
        }

        let key = (
            tls.value.certificate_chain_path.clone(),
            tls.value.private_key_path.clone(),
        );
        let certificate_name =
            if let Some((name, certificate_index)) = self.certificate_names.get(&key).cloned() {
                self.record(
                    CanonicalPath::indexed("certificates", certificate_index),
                    provenance_sources(&tls.provenance),
                );
                name
            } else {
                let certificate_index = self.draft.certificates.len();
                let name = format!("haproxy-cert-{}", certificate_index + 1);
                self.draft.certificates.push(Certificate {
                    name: name.clone(),
                    dns_names: tls.value.dns_names.clone(),
                    source: CertificateSource::Files {
                        certificate_chain_path: tls.value.certificate_chain_path.clone(),
                        private_key_path: tls.value.private_key_path.clone(),
                    },
                });
                self.certificate_names
                    .insert(key, (name.clone(), certificate_index));
                self.record(
                    CanonicalPath::indexed("certificates", certificate_index),
                    provenance_sources(&tls.provenance),
                );
                name
            };

        let profile_index = self.draft.tls_profiles.len();
        let profile_name = format!("haproxy-tls-{}", listener_index + 1);
        self.draft.tls_profiles.push(TlsProfile {
            name: profile_name.clone(),
            certificates: vec![certificate_name.clone()],
            default_certificate: certificate_name,
            min_version: match tls.value.minimum_version {
                TlsMinimumVersion::Tls12 => TlsVersion::Tls12,
                TlsMinimumVersion::Tls13 => TlsVersion::Tls13,
            },
            alpn: tls
                .value
                .alpn
                .iter()
                .map(|protocol| match protocol {
                    TlsAlpn::H2 => AlpnProtocol::H2,
                    TlsAlpn::Http11 => AlpnProtocol::Http11,
                })
                .collect(),
        });
        self.record(
            CanonicalPath::indexed("tls_profiles", profile_index),
            provenance_sources(&tls.provenance),
        );
        Some(profile_name)
    }
}
