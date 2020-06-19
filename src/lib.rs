//!`system-deps` lets you write system dependencies in `Cargo.toml` metadata,
//! rather than programmatically in `build.rs`. This makes those dependencies
//! declarative, so other tools can read them as well.
//! Then, to declare a dependency on `testlib >= 1.2`, a conditional dependency
//! on `testdata >= 4.5` and a dependency on `glib-2.0 >= 2.64`
//! add the following section:
//!
//! ```toml
//! [package.metadata.system-deps]
//! testlib = "1.2"
//! testdata = { version = "4.5", feature = "use-testdata" }
//! glib = { name = "glib-2.0", version = "2.64" }
//! ```
//!
//! In your `build.rs`, add:
//!
//! ```should_panic
//! fn main() {
//!     system_deps::Config::new().probe().unwrap();
//! }
//! ```
//!
//! Dependency versions can also be controlled using features:
//!
//! ```toml
//! [features]
//! v1_2 = []
//! v1_4 = ["v1_4"]
//!
//! [package.system-deps]
//! gstreamer = { name = "gstreamer-1.0", version = "1.0", feature-versions = { v1_2 = "1.2", v1_4 = "1.4" }}
//! ```
//!
//! In this case the highest version among enabled features will be used.

#![deny(missing_docs)]

#[cfg(test)]
#[macro_use]
extern crate lazy_static;

#[cfg(test)]
mod test;

use heck::ShoutySnakeCase;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use strum_macros::EnumString;
use thiserror::Error;
use version_compare::VersionCompare;

/// system-deps errors
#[derive(Error, Debug)]
pub enum Error {
    /// pkg-config error
    #[error(transparent)]
    PkgConfig(#[from] pkg_config::Error),
    /// One of the `Config::add_build_internal` closures failed
    #[error("Failed to build {0}: {1}")]
    BuildInternalClosureError(String, #[source] BuildInternalClosureError),
    /// Failed to read `Cargo.toml`
    #[error("{0}")]
    FailToRead(String, #[source] std::io::Error),
    /// Raised when an error is detected in the metadata defined in `Cargo.toml`
    #[error("{0}")]
    InvalidMetadata(String),
    /// Raised when dependency defined manually using `SYSTEM_DEPS_$NAME_NO_PKG_CONFIG`
    /// did not define at least one lib using `SYSTEM_DEPS_$NAME_LIB` or
    /// `SYSTEM_DEPS_$NAME_LIB_FRAMEWORK`
    #[error("You should define at least one lib using {} or {}", flag_override_var(.0, "LIB"), flag_override_var(.0, "LIB_FRAMEWORK"))]
    MissingLib(String),
    /// An environnement variable in the form of `SYSTEM_DEPS_$NAME_BUILD_INTERNAL`
    /// contained an invalid value (allowed: `auto`, `always`, `never`)
    #[error("{0}")]
    BuildInternalInvalid(String),
    /// system-deps has been asked to internally build a lib, through
    /// `SYSTEM_DEPS_$NAME_BUILD_INTERNAL=always' or `SYSTEM_DEPS_$NAME_BUILD_INTERNAL=auto',
    /// but not closure has been defined using `Config::add_build_internal` to build
    /// this lib
    #[error("Missing build internal closure for {0} (version {1})")]
    BuildInternalNoClosure(String, String),
    /// The library which has been build internally does not match the
    /// required version defined in `Cargo.toml`
    #[error("Internally built {0} {1} but minimum required version is {2}")]
    BuildInternalWrongVersion(String, String, String),
}

#[derive(Error, Debug)]
/// Error used in return value of `Config::add_build_internal` closures
pub enum BuildInternalClosureError {
    /// `pkg-config` error
    #[error(transparent)]
    PkgConfig(#[from] pkg_config::Error),
    /// General failure
    #[error("{0}")]
    Failed(String),
}

impl BuildInternalClosureError {
    /// Create a new `BuildInternalClosureError::Failed` representing a general
    /// failure.
    ///
    /// # Arguments
    ///
    /// * `details`: human-readable details about the failure
    pub fn failed(details: &str) -> Self {
        Self::Failed(details.to_string())
    }
}

type FnBuildInternal = dyn FnOnce(&str) -> std::result::Result<Library, BuildInternalClosureError>;

/// Structure used to configure `metadata` before starting to probe for dependencies
pub struct Config {
    env: EnvVariables,
    build_internals: HashMap<String, Box<FnBuildInternal>>,
}

impl Default for Config {
    fn default() -> Self {
        Self::new_with_env(EnvVariables::Environnement)
    }
}

impl Config {
    /// Create a new set of configuration
    pub fn new() -> Self {
        Self::default()
    }

    fn new_with_env(env: EnvVariables) -> Self {
        Self {
            env,
            build_internals: HashMap::new(),
        }
    }

    /// Probe all libraries configured in the Cargo.toml
    /// `[package.metadata.system-deps]` section.
    pub fn probe(self) -> Result<HashMap<String, Library>, Error> {
        let (libraries, flags) = self.probe_full()?;

        // Output cargo flags
        println!("{}", flags);

        Ok(libraries)
    }

    /// Add hook so system-deps can internally build library `name` if requested by user.
    ///
    /// It will only be triggered if the environnement variable
    /// `SYSTEM_DEPS_$NAME_BUILD_INTERNAL` is defined with either `always` or
    /// `auto` as value. In the latter case, `func` is called only if the requested
    /// version of the library was not found on the system.
    ///
    /// # Arguments
    /// * `name`: the name of the library, as defined in `Cargo.toml`
    /// * `func`: closure called when internally building the library.
    /// It receives as argument the minimum library version required.
    pub fn add_build_internal<F>(self, name: &str, func: F) -> Self
    where
        F: 'static + FnOnce(&str) -> std::result::Result<Library, BuildInternalClosureError>,
    {
        let mut build_internals = self.build_internals;
        build_internals.insert(name.to_string(), Box::new(func));

        Self {
            env: self.env,
            build_internals,
        }
    }

    fn probe_full(mut self) -> Result<(HashMap<String, Library>, BuildFlags), Error> {
        let mut libraries = self.probe_pkg_config()?;
        self.override_from_flags(&mut libraries);
        let flags = self.gen_flags(&libraries)?;

        Ok((libraries, flags))
    }

    fn probe_pkg_config(&mut self) -> Result<HashMap<String, Library>, Error> {
        let dir = self
            .env
            .get("CARGO_MANIFEST_DIR")
            .ok_or_else(|| Error::InvalidMetadata("$CARGO_MANIFEST_DIR not set".into()))?;
        let mut path = PathBuf::from(dir);
        path.push("Cargo.toml");
        let mut manifest = fs::File::open(&path)
            .map_err(|e| Error::FailToRead(format!("Error opening {}", path.display()), e))?;
        let mut manifest_str = String::new();
        manifest
            .read_to_string(&mut manifest_str)
            .map_err(|e| Error::FailToRead(format!("Error reading {}", path.display()), e))?;
        let toml = manifest_str.parse::<toml::Value>().map_err(|e| {
            Error::InvalidMetadata(format!(
                "Error parsing TOML from {}: {:?}",
                path.display(),
                e
            ))
        })?;
        let key = "package.metadata.system-deps";
        let meta = toml
            .get("package")
            .and_then(|v| v.get("metadata"))
            .and_then(|v| v.get("system-deps"))
            .ok_or_else(|| Error::InvalidMetadata(format!("No {} in {}", key, path.display())))?;
        let table = meta.as_table().ok_or_else(|| {
            Error::InvalidMetadata(format!("{} not a table in {}", key, path.display()))
        })?;
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
                                            if self.has_feature(&k) {
                                                enabled_feature_versions.push(feat_vers);
                                            }
                                        }
                                        _ => {
                                            return Err(Error::InvalidMetadata(format!(
                                                "Unexpected feature-version key: {} type {}",
                                                k,
                                                v.type_str()
                                            )))
                                        }
                                    }
                                }
                            }
                            _ => {
                                return Err(Error::InvalidMetadata(format!(
                                    "Unexpected key {}.{}.{} type {}",
                                    key,
                                    name,
                                    tname,
                                    tvalue.type_str()
                                )))
                            }
                        }
                    }
                    if let Some(feature) = feature {
                        if !self.has_feature(feature) {
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
                        version.ok_or_else(|| {
                            Error::InvalidMetadata(format!("No version in {}.{}", key, name))
                        })?,
                    )
                }
                _ => {
                    return Err(Error::InvalidMetadata(format!(
                        "{}.{} not a string or table",
                        key, name
                    )))
                }
            };

            let build_internal = self.get_build_internal_status(name)?;

            let library = if self.env.contains(&flag_override_var(name, "NO_PKG_CONFIG")) {
                Library::from_env_variables()
            } else if build_internal == BuildInternal::Always {
                self.call_build_internal(name, version)?
            } else {
                match pkg_config::Config::new()
                    .atleast_version(&version)
                    .print_system_libs(false)
                    .cargo_metadata(false)
                    .probe(lib_name)
                {
                    Ok(lib) => Library::from_pkg_config(lib),
                    Err(e) => {
                        if build_internal == BuildInternal::Auto {
                            // Try building the lib internally as a fallback
                            self.call_build_internal(name, version)?
                        } else {
                            return Err(e.into());
                        }
                    }
                }
            };

            libraries.insert(name.clone(), library);
        }
        Ok(libraries)
    }

    fn get_build_internal_status(&self, name: &str) -> Result<BuildInternal, Error> {
        let var = flag_override_var(name, "BUILD_INTERNAL");
        let b = match self.env.get(&var).as_deref() {
            Some(s) => BuildInternal::from_str(s).map_err(|_| {
                Error::BuildInternalInvalid(format!(
                    "Invalid value in {}: {} (allowed: 'auto', 'always', 'never')",
                    var, s
                ))
            })?,
            None => BuildInternal::default(),
        };

        Ok(b)
    }

    fn call_build_internal(&mut self, name: &str, version: &str) -> Result<Library, Error> {
        let lib = match self.build_internals.remove(name) {
            Some(f) => f(version).map_err(|e| Error::BuildInternalClosureError(name.into(), e))?,
            None => return Err(Error::BuildInternalNoClosure(name.into(), version.into())),
        };

        // Check that the lib built internally matches the required version
        match VersionCompare::compare(&lib.version, version) {
            Ok(version_compare::CompOp::Lt) => Err(Error::BuildInternalWrongVersion(
                name.into(),
                lib.version.clone(),
                version.into(),
            )),
            _ => Ok(lib),
        }
    }

    fn override_from_flags(&self, libraries: &mut HashMap<String, Library>) {
        for (name, lib) in libraries.iter_mut() {
            if let Some(value) = self.env.get(&flag_override_var(name, "SEARCH_NATIVE")) {
                lib.link_paths = split_paths(&value);
            }
            if let Some(value) = self.env.get(&flag_override_var(name, "SEARCH_FRAMEWORK")) {
                lib.framework_paths = split_paths(&value);
            }
            if let Some(value) = self.env.get(&flag_override_var(name, "LIB")) {
                lib.libs = split_string(&value);
            }
            if let Some(value) = self.env.get(&flag_override_var(name, "LIB_FRAMEWORK")) {
                lib.frameworks = split_string(&value);
            }
            if let Some(value) = self.env.get(&flag_override_var(name, "INCLUDE")) {
                lib.include_paths = split_paths(&value);
            }
        }
    }

    fn gen_flags(&self, libraries: &HashMap<String, Library>) -> Result<BuildFlags, Error> {
        let mut flags = BuildFlags::new();
        let mut include_paths = Vec::new();

        for (name, lib) in libraries.iter() {
            include_paths.extend(lib.include_paths.clone());

            if lib.source == Source::EnvVariables
                && lib.libs.is_empty()
                && lib.frameworks.is_empty()
            {
                return Err(Error::MissingLib(name.clone()));
            }

            lib.link_paths
                .iter()
                .for_each(|l| flags.add(BuildFlag::SearchNative(l.to_string_lossy().to_string())));
            lib.framework_paths.iter().for_each(|f| {
                flags.add(BuildFlag::SearchFramework(f.to_string_lossy().to_string()))
            });
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

        Ok(flags)
    }

    fn has_feature(&self, feature: &str) -> bool {
        let var = format!("CARGO_FEATURE_{}", feature.to_uppercase().replace('-', "_"));
        self.env.contains(&var)
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

    /// Create a `Library` by probing `pkg-config` on an internal directory.
    /// This helper is meant to be used by `Config::add_build_internal` closures
    /// after having built the lib to return the library information to system-deps.
    ///
    /// # Arguments
    ///
    /// * `pkg_config_dir`: the directory where the library `.pc` file is located
    /// * `lib`: the name of the library to look for
    /// * `version`: the minimum version of `lib` required
    ///
    /// # Examples
    ///
    /// ```
    /// let mut config = system_deps::Config::new();
    /// config.add_build_internal("mylib", |version| {
    ///   // Actually build the library here
    ///   system_deps::Library::from_internal_pkg_config("build-dir",
    ///       "mylib", version)
    /// });
    /// ```
    pub fn from_internal_pkg_config<P>(
        pkg_config_dir: P,
        lib: &str,
        version: &str,
    ) -> Result<Self, BuildInternalClosureError>
    where
        P: AsRef<Path>,
    {
        // save current PKG_CONFIG_PATH so we can restore it
        let old = env::var("PKG_CONFIG_PATH");

        match old {
            Ok(ref s) => {
                let paths = [s, &pkg_config_dir.as_ref().to_string_lossy().to_string()];
                let paths = env::join_paths(paths.iter()).unwrap();
                env::set_var("PKG_CONFIG_PATH", paths)
            }
            Err(_) => env::set_var("PKG_CONFIG_PATH", pkg_config_dir.as_ref()),
        }

        let lib = pkg_config::Config::new()
            .atleast_version(&version)
            .print_system_libs(false)
            .cargo_metadata(false)
            .probe(lib);

        env::set_var("PKG_CONFIG_PATH", &old.unwrap_or_else(|_| "".into()));

        match lib {
            Ok(lib) => Ok(Self::from_pkg_config(lib)),
            Err(e) => Err(e.into()),
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

fn flag_override_var(lib: &str, flag: &str) -> String {
    format!("SYSTEM_DEPS_{}_{}", lib.to_shouty_snake_case(), flag)
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

#[derive(Debug, PartialEq, EnumString)]
#[strum(serialize_all = "snake_case")]
enum BuildInternal {
    Auto,
    Always,
    Never,
}

impl Default for BuildInternal {
    fn default() -> Self {
        BuildInternal::Never
    }
}
