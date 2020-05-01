use pkg_config;
use std::env;
use std::sync::Mutex;

use super::{probe_full, BuildFlags, ErrorKind, Result};

lazy_static! {
    static ref LOCK: Mutex<()> = Mutex::new(());
}

fn toml(
    path: &str,
) -> Result<(
    std::collections::HashMap<String, pkg_config::Library>,
    BuildFlags,
)> {
    let _l = LOCK.lock();
    env::set_var(
        "PKG_CONFIG_PATH",
        &env::current_dir().unwrap().join("src").join("tests"),
    );
    env::set_var(
        "CARGO_MANIFEST_DIR",
        &env::current_dir()
            .unwrap()
            .join("src")
            .join("tests")
            .join(path),
    );
    env::set_var("CARGO_FEATURE_TEST_FEATURE", "");
    probe_full()
}

#[test]
fn good() {
    let (libraries, flags) = toml("toml-good").unwrap();
    let testlib = libraries.get("testlib").unwrap();
    assert_eq!(testlib.version, "1.2.3");
    let testdata = libraries.get("testdata").unwrap();
    assert_eq!(testdata.version, "4.5.6");
    assert!(libraries.get("testmore").is_none());

    assert_eq!(flags.to_string(), "cargo:include=/usr/include/testlib\n");
}

fn toml_err(path: &str, err_starts_with: &str) {
    let err = toml(path).unwrap_err();
    if !err.description().starts_with(err_starts_with) {
        panic!(
            "Expected error to start with: {:?}\nGot error: {:?}",
            err_starts_with, err
        );
    }
}

// Assert a PkgConfig error because requested lib version cannot be found
fn toml_pkg_config_err_version(path: &str, expected_version: &str) {
    let err = toml(path).unwrap_err();
    match err.kind() {
        ErrorKind::PkgConfig(e) => match e {
            pkg_config::Error::Failure {
                command: cmd,
                output: _,
            } => {
                let s = format!(">= {}\"", expected_version);
                assert!(cmd.ends_with(&s));
            }
            _ => panic!("Wrong pkg-config error type"),
        },
        _ => panic!("Wrong error type"),
    }
}

#[test]
fn missing_file() {
    toml_err("toml-missing-file", "Error opening");
}

#[test]
fn missing_key() {
    toml_err("toml-missing-key", "No package.metadata.pkg-config in");
}

#[test]
fn not_table() {
    toml_err(
        "toml-not-table",
        "package.metadata.pkg-config not a table in",
    );
}

#[test]
fn version_missing() {
    toml_err(
        "toml-version-missing",
        "No version in package.metadata.pkg-config.testlib",
    );
}

#[test]
fn version_not_string() {
    toml_err(
        "toml-version-not-string",
        "package.metadata.pkg-config.testlib not a string or table",
    );
}

#[test]
fn version_in_table_not_string() {
    toml_err(
        "toml-version-in-table-not-string",
        "Unexpected key package.metadata.pkg-config.testlib.version type integer",
    );
}

#[test]
fn feature_not_string() {
    toml_err(
        "toml-feature-not-string",
        "Unexpected key package.metadata.pkg-config.testlib.feature type integer",
    );
}

#[test]
fn unexpected_key() {
    toml_err(
        "toml-unexpected-key",
        "Unexpected key package.metadata.pkg-config.testlib.color type string",
    );
}

#[test]
fn override_name() {
    let (libraries, _) = toml("toml-override-name").unwrap();
    let testlib = libraries.get("testlib").unwrap();
    assert_eq!(testlib.version, "2.0.0");
}

#[test]
fn feature_versions() {
    let (libraries, _) = toml("toml-feature-versions").unwrap();
    let testdata = libraries.get("testdata").unwrap();
    assert_eq!(testdata.version, "4.5.6");

    // version 5 is not available
    env::set_var("CARGO_FEATURE_V5", "");
    toml_pkg_config_err_version("toml-feature-versions", "5");

    // We check the highest version enabled by features
    env::set_var("CARGO_FEATURE_V6", "");
    toml_pkg_config_err_version("toml-feature-versions", "6");
}
