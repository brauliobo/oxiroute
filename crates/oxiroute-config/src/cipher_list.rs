pub const RUSTLS_TLS12_SUITES: [&str; 6] = [
    "ECDHE-ECDSA-AES128-GCM-SHA256",
    "ECDHE-RSA-AES128-GCM-SHA256",
    "ECDHE-ECDSA-AES256-GCM-SHA384",
    "ECDHE-RSA-AES256-GCM-SHA384",
    "ECDHE-ECDSA-CHACHA20-POLY1305",
    "ECDHE-RSA-CHACHA20-POLY1305",
];

const RUSTLS_TLS13_SUITES: [&str; 3] = [
    "TLS_AES_128_GCM_SHA256",
    "TLS_AES_256_GCM_SHA384",
    "TLS_CHACHA20_POLY1305_SHA256",
];

const DHE_RSA_PREFIXES: [&str; 2] = ["DHE-RSA-", "EDH-RSA-"];

const OPENSSL_KEYWORDS: &[&str] = &[
    "DEFAULT",
    "COMPLEMENTOFDEFAULT",
    "ALL",
    "COMPLEMENTOFALL",
    "HIGH",
    "MEDIUM",
    "LOW",
    "EXP",
    "EXPORT",
    "EXPORT40",
    "EXPORT56",
    "eNULL",
    "NULL",
    "aNULL",
    "kRSA",
    "aRSA",
    "RSA",
    "kDHE",
    "kEDH",
    "DH",
    "DHE",
    "EDH",
    "ADH",
    "kDHEPSK",
    "kECDHE",
    "kEECDH",
    "ECDH",
    "ECDHE",
    "EECDH",
    "AECDH",
    "aDSS",
    "DSS",
    "aECDSA",
    "ECDSA",
    "kPSK",
    "kRSAPSK",
    "kECDHEPSK",
    "PSK",
    "aPSK",
    "kSRP",
    "SRP",
    "aGOST",
    "aGOST01",
    "aGOST12",
    "kGOST",
    "GOST94",
    "GOST89MAC",
    "GOST89MAC12",
    "GOST2012",
    "AES128",
    "AES256",
    "AES",
    "AESGCM",
    "AESCCM",
    "AESCCM8",
    "ARIA128",
    "ARIA256",
    "ARIA",
    "CAMELLIA128",
    "CAMELLIA256",
    "CAMELLIA",
    "CHACHA20",
    "3DES",
    "DES",
    "RC4",
    "RC2",
    "IDEA",
    "SEED",
    "MD5",
    "SHA1",
    "SHA",
    "SHA256",
    "SHA384",
    "SSLv3",
    "TLSv1",
    "TLSv1.0",
    "TLSv1.1",
    "TLSv1.2",
    "FIPS",
    "SUITEB128",
    "SUITEB128ONLY",
    "SUITEB192",
];

/// An OpenSSL cipher string split into the suite names it lists and the keywords or operators that only OpenSSL can
/// expand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CipherList<'a> {
    suites: Vec<&'a str>,
    macros: Vec<&'a str>,
}

impl<'a> CipherList<'a> {
    #[must_use]
    pub fn parse(value: &'a str) -> Self {
        let (macros, suites) = value
            .split([':', ' ', ',', ';'])
            .filter(|token| !token.is_empty())
            .partition(|token| is_macro(token));
        Self { suites, macros }
    }

    #[must_use]
    pub fn suites(&self) -> &[&'a str] {
        &self.suites
    }

    #[must_use]
    pub fn macros(&self) -> &[&'a str] {
        &self.macros
    }

    #[must_use]
    pub fn has_macros(&self) -> bool {
        !self.macros.is_empty()
    }

    #[must_use]
    pub fn lists_dhe_rsa(&self) -> bool {
        self.suites.iter().any(|suite| {
            DHE_RSA_PREFIXES
                .iter()
                .any(|prefix| suite.starts_with(prefix))
        })
    }

    #[must_use]
    pub fn lists_rustls_tls12_suite(&self) -> bool {
        self.suites
            .iter()
            .any(|suite| RUSTLS_TLS12_SUITES.contains(suite))
    }

    pub fn suites_rustls_cannot_serve(&self) -> impl Iterator<Item = &'a str> {
        self.suites.iter().copied().filter(|suite| {
            !RUSTLS_TLS12_SUITES.contains(suite) && !RUSTLS_TLS13_SUITES.contains(suite)
        })
    }
}

fn is_macro(token: &str) -> bool {
    token.starts_with(['!', '+', '-', '@']) || token.contains(['+', '@']) || is_keyword(token)
}

fn is_keyword(token: &str) -> bool {
    OPENSSL_KEYWORDS.contains(&token) || token.starts_with("PROFILE=")
}
