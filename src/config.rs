//! Configuration: one judgment, file precedence.
//!
//! The command line (`--config PATH`) or the environment (`PORTAL_DOWNLOAD_CONFIG=PATH`)
//! only select the file; all values come from that one file. Without either, the first
//! existing file of the user-level and system-level places is read. If no file is found,
//! every value is its default, and `check` says so.
//!
//! Empty values: an empty `listen` means the default (reported as "default (set empty)");
//! an empty `state_dir` aborts the start, because a silent fallback would put the state
//! somewhere the operator did not intend.

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const ENV_CONFIG: &str = "PORTAL_DOWNLOAD_CONFIG";
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8080";

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    listen: Option<String>,
    state_dir: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Value<T> {
    pub value: T,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// The file the values came from, or `None` if every value is a default.
    pub file: Option<PathBuf>,
    /// How the file was selected.
    pub file_source: String,
    pub listen: Value<String>,
    pub state_dir: Value<PathBuf>,
}

#[derive(Debug)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn user_config_path() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(x).join("portal-download/config.toml"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/portal-download/config.toml"))
}

fn default_state_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(x).join("portal-download");
    }
    match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h).join(".local/state/portal-download"),
        None => PathBuf::from("/var/lib/portal-download"),
    }
}

/// Selects the file: command line, then environment, then user-level, then system-level place.
fn select_file(cli: Option<&Path>) -> Result<(Option<PathBuf>, String), ConfigError> {
    if let Some(p) = cli {
        if !p.is_file() {
            return Err(ConfigError(format!(
                "The configuration file {} named with --config does not exist. Create it (see examples/config.toml) or leave out --config to use the defaults.",
                p.display()
            )));
        }
        return Ok((Some(p.to_path_buf()), "command line (--config)".into()));
    }
    if let Some(v) = std::env::var_os(ENV_CONFIG) {
        if v.is_empty() {
            return Err(ConfigError(format!(
                "{ENV_CONFIG} is set but empty. Set it to the path of a configuration file, or unset it to use the user-level or system-level place."
            )));
        }
        let p = PathBuf::from(v);
        if !p.is_file() {
            return Err(ConfigError(format!(
                "The configuration file {} named in {ENV_CONFIG} does not exist. Create it or correct the variable.",
                p.display()
            )));
        }
        return Ok((Some(p), format!("environment ({ENV_CONFIG})")));
    }
    if let Some(p) = user_config_path().filter(|p| p.is_file()) {
        return Ok((Some(p), "user-level place".into()));
    }
    let sys = PathBuf::from("/etc/portal-download/config.toml");
    if sys.is_file() {
        return Ok((Some(sys), "system-level place".into()));
    }
    Ok((None, "no file found; every value is its default".into()))
}

pub fn load(cli: Option<&Path>) -> Result<Config, ConfigError> {
    let (file, file_source) = select_file(cli)?;
    let parsed: FileConfig = match &file {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|e| ConfigError(format!("The configuration file {} cannot be read: {e}. Check its permissions.", p.display())))?;
            toml::from_str(&text).map_err(|e| {
                ConfigError(format!(
                    "The configuration file {} is not valid: {e}. Allowed keys are `listen` and `state_dir`; see examples/config.toml.",
                    p.display()
                ))
            })?
        }
        None => FileConfig::default(),
    };
    let from = |set: bool| -> String {
        match (&file, set) {
            (Some(p), true) => format!("file {}", p.display()),
            _ => "default".into(),
        }
    };

    let listen = match parsed.listen {
        Some(s) if s.trim().is_empty() => Value { value: DEFAULT_LISTEN.into(), source: "default (set empty)".into() },
        Some(s) => Value { value: s.trim().to_string(), source: from(true) },
        None => Value { value: DEFAULT_LISTEN.into(), source: from(false) },
    };
    if listen.value.parse::<std::net::SocketAddr>().is_err() {
        return Err(ConfigError(format!(
            "`listen` = \"{}\" ({}) is not an address with port. Write it as IP:PORT, for example \"127.0.0.1:8080\".",
            listen.value, listen.source
        )));
    }

    let state_dir = match parsed.state_dir {
        Some(s) if s.trim().is_empty() => {
            return Err(ConfigError(format!(
                "`state_dir` is set but empty in {}. Set it to the directory that holds the database and the setup file, or remove the key to use the default {}.",
                file.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
                default_state_dir().display()
            )))
        }
        Some(s) => Value { value: PathBuf::from(s.trim()), source: from(true) },
        None => Value { value: default_state_dir(), source: from(false) },
    };

    Ok(Config { file, file_source, listen, state_dir })
}

impl Config {
    /// The report of the check: every value with where it came from.
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({
            "file": self.file.as_ref().map(|p| p.display().to_string()),
            "file_source": self.file_source,
            "listen": { "value": self.listen.value, "source": self.listen.source },
            "state_dir": { "value": self.state_dir.value.display().to_string(), "source": self.state_dir.source },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("c.toml");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn values_come_from_the_selected_file() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "listen = \"127.0.0.1:9000\"\nstate_dir = \"/tmp/x\"\n");
        let c = load(Some(&p)).unwrap();
        assert_eq!(c.listen.value, "127.0.0.1:9000");
        assert!(c.listen.source.starts_with("file "));
        assert_eq!(c.state_dir.value, PathBuf::from("/tmp/x"));
    }

    #[test]
    fn empty_listen_is_the_default_and_says_so() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "listen = \"\"\n");
        let c = load(Some(&p)).unwrap();
        assert_eq!(c.listen.value, DEFAULT_LISTEN);
        assert_eq!(c.listen.source, "default (set empty)");
    }

    #[test]
    fn empty_state_dir_aborts() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "state_dir = \" \"\n");
        let e = load(Some(&p)).unwrap_err();
        assert!(e.0.contains("state_dir"));
    }

    #[test]
    fn unknown_key_is_rejected_with_the_allowed_keys() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "port = 1\n");
        let e = load(Some(&p)).unwrap_err();
        assert!(e.0.contains("Allowed keys"));
    }

    #[test]
    fn missing_named_file_is_an_error() {
        let e = load(Some(Path::new("/nonexistent/portal.toml"))).unwrap_err();
        assert!(e.0.contains("does not exist"));
    }
}
