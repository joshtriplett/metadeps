//! metadeps lets you write `pkg-config` dependencies in `Cargo.toml` metadata,
//! rather than programmatically in `build.rs`.  This makes those dependencies
//! declarative, so other tools can read them as well.
//!
//! metadeps parses metadata like this in `Cargo.toml`:
//!
//! ```toml
//! [package.metadata.pkg-config]
//! testlib = "1.2"
//! testdata = { version = "4.5", feature = "some-feature" }
//! ```

#![deny(missing_docs, warnings)]

extern crate anyhow;
extern crate pkg_config;
extern crate toml;

pub use anyhow::{Error, Result};

use anyhow::Context;
use pkg_config::{Config, Library};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Read;
use std::path::Path;

const KEY: &str = "package.metadata.pkg-config";

fn get_pkgconfig_table(toml: &toml::Value) -> Option<&toml::Value> {
    toml.get("package")?.get("metadata")?.get("pkg-config")
}

/// Probe all libraries configured in the Cargo.toml
/// `[package.metadata.pkg-config]` section.
pub fn probe() -> Result<HashMap<String, Library>> {
    let dir = env::var_os("CARGO_MANIFEST_DIR")
        .ok_or_else(|| anyhow::anyhow!("$CARGO_MANIFEST_DIR not set"))?;
    let path = Path::new(&dir).join("Cargo.toml");
    let mut manifest_str = String::new();
    fs::File::open(&path)
        .with_context(|| format!("Error opening {}", path.display()))?
        .read_to_string(&mut manifest_str)
        .with_context(|| format!("Error reading {}", path.display()))?;
    let toml = manifest_str
        .parse::<toml::Value>()
        .with_context(|| format!("Error parsing TOML from {}", path.display()))?;
    let table = get_pkgconfig_table(&toml)
        .with_context(|| format!("No {} key in {}", KEY, path.display()))?
        .as_table()
        .with_context(|| format!("{} not a table in {}", KEY, path.display()))?;
    let mut libraries = HashMap::new();
    for (name, value) in table {
        let ref version = match value {
            &toml::Value::String(ref s) => s,
            &toml::Value::Table(ref t) => {
                let mut feature = None;
                let mut version = None;
                for (tname, tvalue) in t {
                    match (tname.as_str(), tvalue) {
                        ("feature", &toml::Value::String(ref s)) => {
                            feature = Some(s);
                        }
                        ("version", &toml::Value::String(ref s)) => {
                            version = Some(s);
                        }
                        _ => anyhow::bail!(
                            "Unexpected key {}.{}.{} type {}",
                            KEY,
                            name,
                            tname,
                            tvalue.type_str()
                        ),
                    }
                }
                if let Some(feature) = feature {
                    let var = format!("CARGO_FEATURE_{}", feature.to_uppercase().replace('-', "_"));
                    if env::var_os(var).is_none() {
                        continue;
                    }
                }
                version.ok_or_else(|| anyhow::anyhow!("No version in {}.{}", KEY, name))?
            }
            _ => anyhow::bail!("{}.{} not a string or table", KEY, name),
        };
        let library = Config::new().atleast_version(&version).probe(name)?;
        libraries.insert(name.clone(), library);
    }
    Ok(libraries)
}
