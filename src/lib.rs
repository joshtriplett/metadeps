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
//! glib = { name = "glib-2.0", version = "2.64" }
//! gstreamer = { name = "gstreamer-1.0", version = "1.0", feature-versions = { v1_2 = "1.2", v1_4 = "1.4" }}
//! ```

#![deny(missing_docs)]

#[macro_use]
extern crate error_chain;

#[cfg(test)]
#[macro_use]
extern crate lazy_static;

#[cfg(test)]
mod test;

use heck::ShoutySnakeCase;
use pkg_config::Config;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use version_compare::VersionCompare;

error_chain! {
    foreign_links {
        PkgConfig(pkg_config::Error) #[doc="pkg-config error"];
    }
}

#[derive(Debug, PartialEq)]
/// From where the library settings have been retrieved
pub enum Source {
    /// Settings have been retrieved from `pkg-config`
    PkgConfig,
    /// Settings have been defined using user defined environnement variables
    EnvVariables,
}

#[derive(Debug)]
/// A system dependency
pub struct Library {
    /// From where the library settings have been retrieved
    pub source: Source,
    /// libraries the linker should link on
    pub libs: Vec<String>,
    /// directories where the compiler should look for libraries
    pub link_paths: Vec<PathBuf>,
    /// frameworks the linker should link on
    pub frameworks: Vec<String>,
    /// directories where the compiler should look for frameworks
    pub framework_paths: Vec<PathBuf>,
    /// directories where the compiler should look for header files
    pub include_paths: Vec<PathBuf>,
    /// macros that should be defined by the compiler
    pub defines: HashMap<String, Option<String>>,
    /// library version
    pub version: String,
}

impl Library {
    fn from_pkg_config(l: pkg_config::Library) -> Self {
        Self {
            source: Source::PkgConfig,
            libs: l.libs,
            link_paths: l.link_paths,
            include_paths: l.include_paths,
            frameworks: l.frameworks,
            framework_paths: l.framework_paths,
            defines: l.defines,
            version: l.version,
        }
    }

    fn from_env_variables() -> Self {
        Self {
            source: Source::EnvVariables,
            libs: Vec::new(),
            link_paths: Vec::new(),
            include_paths: Vec::new(),
            frameworks: Vec::new(),
            framework_paths: Vec::new(),
            defines: HashMap::new(),
            version: String::new(),
        }
    }
}

#[derive(Debug)]
enum EnvVariables {
    Environnement,
    #[cfg(test)]
    Mock(HashMap<&'static str, String>),
}

impl EnvVariables {
    fn contains(&self, var: &str) -> bool {
        self.get(var).is_some()
    }

    fn get(&self, var: &str) -> Option<String> {
        match self {
            EnvVariables::Environnement => env::var(var).ok(),
            #[cfg(test)]
            EnvVariables::Mock(vars) => vars.get(var).cloned(),
        }
    }
}

fn has_feature(env_vars: &EnvVariables, feature: &str) -> bool {
    let var = format!("CARGO_FEATURE_{}", feature.to_uppercase().replace('-', "_"));
    env_vars.contains(&var)
}

fn probe_pkg_config(env_vars: &EnvVariables) -> Result<HashMap<String, Library>> {
    let dir = env_vars
        .get("CARGO_MANIFEST_DIR")
        .ok_or("$CARGO_MANIFEST_DIR not set")?;
    let mut path = PathBuf::from(dir);
    path.push("Cargo.toml");
    let mut manifest =
        fs::File::open(&path).chain_err(|| format!("Error opening {}", path.display()))?;
    let mut manifest_str = String::new();
    manifest
        .read_to_string(&mut manifest_str)
        .chain_err(|| format!("Error reading {}", path.display()))?;
    let toml = manifest_str
        .parse::<toml::Value>()
        .map_err(|e| format!("Error parsing TOML from {}: {:?}", path.display(), e))?;
    let key = "package.metadata.pkg-config";
    let meta = toml
        .get("package")
        .and_then(|v| v.get("metadata"))
        .and_then(|v| v.get("pkg-config"))
        .ok_or(format!("No {} in {}", key, path.display()))?;
    let table = meta
        .as_table()
        .ok_or(format!("{} not a table in {}", key, path.display()))?;
    let mut libraries = HashMap::new();
    for (name, value) in table {
        let (lib_name, version) = match value {
            toml::Value::String(ref s) => (name, s),
            toml::Value::Table(ref t) => {
                let mut feature = None;
                let mut version = None;
                let mut lib_name = None;
                let mut enabled_feature_versions = Vec::new();
                for (tname, tvalue) in t {
                    match (tname.as_str(), tvalue) {
                        ("feature", &toml::Value::String(ref s)) => {
                            feature = Some(s);
                        }
                        ("version", &toml::Value::String(ref s)) => {
                            version = Some(s);
                        }
                        ("name", &toml::Value::String(ref s)) => {
                            lib_name = Some(s);
                        }
                        ("feature-versions", &toml::Value::Table(ref feature_versions)) => {
                            for (k, v) in feature_versions {
                                match (k.as_str(), v) {
                                    (_, &toml::Value::String(ref feat_vers)) => {
                                        if has_feature(&env_vars, &k) {
                                            enabled_feature_versions.push(feat_vers);
                                        }
                                    }
                                    _ => bail!(
                                        "Unexpected feature-version key: {} type {}",
                                        k,
                                        v.type_str()
                                    ),
                                }
                            }
                        }
                        _ => bail!(
                            "Unexpected key {}.{}.{} type {}",
                            key,
                            name,
                            tname,
                            tvalue.type_str()
                        ),
                    }
                }
                if let Some(feature) = feature {
                    if !has_feature(&env_vars, feature) {
                        continue;
                    }
                }

                let version = {
                    // Pick the highest feature enabled version
                    if !enabled_feature_versions.is_empty() {
                        enabled_feature_versions.sort_by(|a, b| {
                            VersionCompare::compare(b, a)
                                .expect("failed to compare versions")
                                .ord()
                                .expect("invalid version")
                        });
                        Some(enabled_feature_versions[0])
                    } else {
                        version
                    }
                };

                (
                    lib_name.unwrap_or(name),
                    version.ok_or(format!("No version in {}.{}", key, name))?,
                )
            }
            _ => bail!("{}.{} not a string or table", key, name),
        };
        let library = if env_vars.contains(&flag_override_var(name, "NO_PKG_CONFIG")) {
            Library::from_env_variables()
        } else {
            Library::from_pkg_config(
                Config::new()
                    .atleast_version(&version)
                    .print_system_libs(false)
                    .cargo_metadata(false)
                    .probe(lib_name)?,
            )
        };

        libraries.insert(name.clone(), library);
    }
    Ok(libraries)
}

// TODO: add support for "rustc-link-lib=static=" ?
#[derive(Debug, PartialEq)]
enum BuildFlag {
    Include(String),
    SearchNative(String),
    SearchFramework(String),
    Lib(String),
    LibFramework(String),
}

impl fmt::Display for BuildFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildFlag::Include(paths) => write!(f, "include={}", paths),
            BuildFlag::SearchNative(lib) => write!(f, "rustc-link-search=native={}", lib),
            BuildFlag::SearchFramework(lib) => write!(f, "rustc-link-search=framework={}", lib),
            BuildFlag::Lib(lib) => write!(f, "rustc-link-lib={}", lib),
            BuildFlag::LibFramework(lib) => write!(f, "rustc-link-lib=framework={}", lib),
        }
    }
}

#[derive(Debug, PartialEq)]
struct BuildFlags(Vec<BuildFlag>);

impl BuildFlags {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn add(&mut self, flag: BuildFlag) {
        self.0.push(flag);
    }
}

impl fmt::Display for BuildFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for flag in self.0.iter() {
            writeln!(f, "cargo:{}", flag)?;
        }
        Ok(())
    }
}

fn gen_flags(libraries: &HashMap<String, Library>) -> BuildFlags {
    let mut flags = BuildFlags::new();
    let mut include_paths = Vec::new();

    for (_name, lib) in libraries.iter() {
        include_paths.extend(lib.include_paths.clone());

        lib.link_paths
            .iter()
            .for_each(|l| flags.add(BuildFlag::SearchNative(l.to_string_lossy().to_string())));
        lib.framework_paths
            .iter()
            .for_each(|f| flags.add(BuildFlag::SearchFramework(f.to_string_lossy().to_string())));
        lib.libs
            .iter()
            .for_each(|l| flags.add(BuildFlag::Lib(l.clone())));
        lib.frameworks
            .iter()
            .for_each(|f| flags.add(BuildFlag::LibFramework(f.clone())));
    }

    // Export DEP_$CRATE_INCLUDE env variable with the headers paths,
    // see https://kornel.ski/rust-sys-crate#headers
    if !include_paths.is_empty() {
        if let Ok(paths) = std::env::join_paths(include_paths) {
            flags.add(BuildFlag::Include(paths.to_string_lossy().to_string()));
        }
    }

    flags
}

fn flag_override_var(lib: &str, flag: &str) -> String {
    format!("METADEPS_{}_{}", lib.to_shouty_snake_case(), flag)
}

fn split_paths(value: &str) -> Vec<PathBuf> {
    if !value.is_empty() {
        let paths = env::split_paths(&value);
        paths.map(|p| Path::new(&p).into()).collect()
    } else {
        Vec::new()
    }
}

fn split_string(value: &str) -> Vec<String> {
    if !value.is_empty() {
        value.split(' ').map(|s| s.to_string()).collect()
    } else {
        Vec::new()
    }
}

fn override_from_flags(env_vars: &EnvVariables, libraries: &mut HashMap<String, Library>) {
    for (name, lib) in libraries.iter_mut() {
        if let Some(value) = env_vars.get(&flag_override_var(name, "SEARCH_NATIVE")) {
            lib.link_paths = split_paths(&value);
        }
        if let Some(value) = env_vars.get(&flag_override_var(name, "SEARCH_FRAMEWORK")) {
            lib.framework_paths = split_paths(&value);
        }
        if let Some(value) = env_vars.get(&flag_override_var(name, "LIB")) {
            lib.libs = split_string(&value);
        }
        if let Some(value) = env_vars.get(&flag_override_var(name, "LIB_FRAMEWORK")) {
            lib.frameworks = split_string(&value);
        }
        if let Some(value) = env_vars.get(&flag_override_var(name, "INCLUDE")) {
            lib.include_paths = split_paths(&value);
        }
    }
}

fn probe_full(env: EnvVariables) -> Result<(HashMap<String, Library>, BuildFlags)> {
    let mut libraries = probe_pkg_config(&env)?;
    override_from_flags(&env, &mut libraries);
    let flags = gen_flags(&libraries);

    Ok((libraries, flags))
}

/// Probe all libraries configured in the Cargo.toml
/// `[package.metadata.pkg-config]` section.
pub fn probe() -> Result<HashMap<String, Library>> {
    let env = EnvVariables::Environnement;
    let (libraries, flags) = probe_full(env)?;

    // Output cargo flags
    println!("{}", flags);

    Ok(libraries)
}
