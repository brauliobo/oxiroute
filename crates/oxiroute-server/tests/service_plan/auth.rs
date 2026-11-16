use std::fs;

use oxiroute_config::HttpAccessPolicy;
use oxiroute_server::ServicePlanError;
use tempfile::TempDir;

use crate::canonical::canonical_config;
use crate::config_support::runtime_plan;

#[cfg(unix)]
#[test]
fn basic_auth_rejects_unsupported_htpasswd_hashes() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = TempDir::new().expect("htpasswd fixture");
    let unsupported = directory.path().join("unsupported.htpasswd");
    fs::write(&unsupported, "user:{SHA}VBPuJHI7uixaa6LQGWx4s+5GKNE=\n")
        .expect("unsupported htpasswd");
    fs::set_permissions(&unsupported, fs::Permissions::from_mode(0o600)).expect("htpasswd mode");
    let mut config = canonical_config();
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: unsupported.clone(),
        realm: "private".into(),
    });
    assert!(matches!(
        runtime_plan(&config.clone().validate().unwrap()),
        Err(ServicePlanError::AccessPreflight { .. })
    ));
}

#[cfg(unix)]
#[test]
fn basic_auth_preflights_apr1_and_mixed_scheme_files() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = TempDir::new().expect("htpasswd fixture");
    let mut config = canonical_config();
    let apr1 = directory.path().join("apr1.htpasswd");
    fs::write(&apr1, "myName:$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/\n").expect("APR1 htpasswd");
    fs::set_permissions(&apr1, fs::Permissions::from_mode(0o600)).expect("APR1 mode");
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: apr1.clone(),
        realm: "private".into(),
    });
    runtime_plan(&config.clone().validate().unwrap()).expect("valid APR1 htpasswd");

    fs::set_permissions(&apr1, fs::Permissions::from_mode(0o640))
        .expect("group-readable APR1 mode");
    runtime_plan(&config.clone().validate().unwrap()).expect("group-readable APR1 htpasswd");

    fs::set_permissions(&apr1, fs::Permissions::from_mode(0o644))
        .expect("world-readable APR1 mode");
    assert!(matches!(
        runtime_plan(&config.clone().validate().unwrap()),
        Err(ServicePlanError::AccessPreflight { .. })
    ));

    for (name, hash) in [
        ("long-salt", "$apr1$toolongsalt$HqJZimcKQFAMYayBlzkrA/"),
        ("short-digest", "$apr1$r31.....$HqJZimcKQFAMYayBlzkrA"),
        ("invalid-digest", "$apr1$r31.....$HqJZimcKQFAMYayBlzkrA!"),
        (
            "noncanonical-digest",
            "$apr1$r31.....$HqJZimcKQFAMYayBlzkrAA",
        ),
    ] {
        let malformed_apr1 = directory.path().join(format!("{name}.htpasswd"));
        fs::write(&malformed_apr1, format!("user:{hash}\n")).expect("malformed APR1 htpasswd");
        fs::set_permissions(&malformed_apr1, fs::Permissions::from_mode(0o600))
            .expect("malformed APR1 mode");
        config.http_services[0].routes[0].access_policy =
            Some(HttpAccessPolicy::BasicHtpasswdFile {
                htpasswd_file_path: malformed_apr1,
                realm: "private".into(),
            });
        assert!(matches!(
            runtime_plan(&config.clone().validate().unwrap()),
            Err(ServicePlanError::AccessPreflight { .. })
        ));
    }

    let duplicate_apr1 = directory.path().join("duplicate-apr1.htpasswd");
    fs::write(
        &duplicate_apr1,
        concat!(
            "user:$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/\n",
            "user:$apr1$hfT7jp2q$2F2Tht4XByp/xPQ4H4.vT0\n",
        ),
    )
    .expect("duplicate APR1 htpasswd");
    fs::set_permissions(&duplicate_apr1, fs::Permissions::from_mode(0o600))
        .expect("duplicate APR1 mode");
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: duplicate_apr1,
        realm: "private".into(),
    });
    assert!(matches!(
        runtime_plan(&config.clone().validate().unwrap()),
        Err(ServicePlanError::AccessPreflight { .. })
    ));

    let mixed_schemes = directory.path().join("mixed-schemes.htpasswd");
    fs::write(
        &mixed_schemes,
        concat!(
            "first:$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/\n",
            "second:$2y$05$c4WoMPo3SXsafkva.HHa6uXQZWr7oboPiC2bT/r7q1BB8I2s0BRqC\n",
        ),
    )
    .expect("mixed-scheme htpasswd");
    fs::set_permissions(&mixed_schemes, fs::Permissions::from_mode(0o600))
        .expect("mixed-scheme mode");
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: mixed_schemes,
        realm: "private".into(),
    });
    runtime_plan(&config.clone().validate().unwrap()).expect("mixed-scheme htpasswd");
}

#[cfg(unix)]
#[test]
fn basic_auth_preflights_a_large_multi_user_htpasswd_file() {
    use std::{fmt::Write as _, os::unix::fs::PermissionsExt as _};

    let directory = TempDir::new().expect("htpasswd fixture");
    let path = directory.path().join("large.htpasswd");
    let mut contents = String::with_capacity(1_000_000);
    for index in 0..20_000 {
        writeln!(
            contents,
            "user-{index:05}:$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/"
        )
        .expect("write htpasswd entry");
    }
    assert!(contents.len() < 1024 * 1024);
    fs::write(&path, contents).expect("large htpasswd");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("htpasswd mode");

    let mut config = canonical_config();
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: path,
        realm: "private".into(),
    });
    runtime_plan(&config.clone().validate().unwrap()).expect("large htpasswd preflight");
}

#[cfg(unix)]
#[test]
fn basic_auth_rejects_symlinks_and_excessive_bcrypt_work() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let directory = TempDir::new().expect("htpasswd fixture");
    let mut config = canonical_config();
    let bcrypt = directory.path().join("bcrypt.htpasswd");
    fs::write(
        &bcrypt,
        "user:$2y$05$c4WoMPo3SXsafkva.HHa6uXQZWr7oboPiC2bT/r7q1BB8I2s0BRqC\n",
    )
    .expect("bcrypt htpasswd");
    fs::set_permissions(&bcrypt, fs::Permissions::from_mode(0o600)).expect("bcrypt mode");
    let link = directory.path().join("linked.htpasswd");
    symlink(&bcrypt, &link).expect("htpasswd symlink");
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: link,
        realm: "private".into(),
    });
    assert!(matches!(
        runtime_plan(&config.clone().validate().unwrap()),
        Err(ServicePlanError::AccessPreflight { .. })
    ));

    let excessive = directory.path().join("excessive.htpasswd");
    fs::write(
        &excessive,
        "user:$2y$13$c4WoMPo3SXsafkva.HHa6uXQZWr7oboPiC2bT/r7q1BB8I2s0BRqC\n",
    )
    .expect("excessive bcrypt htpasswd");
    fs::set_permissions(&excessive, fs::Permissions::from_mode(0o600)).expect("htpasswd mode");
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: excessive,
        realm: "private".into(),
    });
    assert!(matches!(
        runtime_plan(&config.clone().validate().unwrap()),
        Err(ServicePlanError::AccessPreflight { .. })
    ));

    let mixed = directory.path().join("mixed-cost.htpasswd");
    fs::write(
        &mixed,
        concat!(
            "first:$2y$05$c4WoMPo3SXsafkva.HHa6uXQZWr7oboPiC2bT/r7q1BB8I2s0BRqC\n",
            "second:$2y$06$c4WoMPo3SXsafkva.HHa6uXQZWr7oboPiC2bT/r7q1BB8I2s0BRqC\n",
        ),
    )
    .expect("mixed bcrypt htpasswd");
    fs::set_permissions(&mixed, fs::Permissions::from_mode(0o600)).expect("htpasswd mode");
    config.http_services[0].routes[0].access_policy = Some(HttpAccessPolicy::BasicHtpasswdFile {
        htpasswd_file_path: mixed,
        realm: "private".into(),
    });
    runtime_plan(&config.clone().validate().unwrap()).expect("mixed-cost bcrypt htpasswd");
}
