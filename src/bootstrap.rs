//! The bootstrap window: a file next to the store holding the setup secret and an expiry.
//!
//! Every start of the service opens the window; `portal-download bootstrap` opens it
//! without a restart. While it is open, the setup secret allows two things only: creating a
//! person with the right to administer people (the first one, or a new one after all
//! administrators lost their devices) and creating a machine key. The file is read on every
//! use; once the expiry has passed the window is closed, nobody has to delete anything.

use crate::error::{OpError, OpResult};
use crate::token;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "bootstrap.toml";
pub const DEFAULT_MINUTES: i64 = 30;

#[derive(Serialize, Deserialize)]
struct WindowFile {
    secret: String,
    expires_at: String,
}

pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

/// Opens the window: writes a fresh secret with an expiry, readable by the owner only.
pub fn open(state_dir: &Path, minutes: i64) -> OpResult<(String, String)> {
    let secret = token::generate("pds_");
    let expires_at = (Utc::now() + Duration::minutes(minutes)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let body = format!(
        "# Setup window of portal-download. Written at start or by `portal-download bootstrap`.\n# Closes by itself at expires_at; nothing needs to be deleted.\n{}",
        toml::to_string(&WindowFile { secret: secret.clone(), expires_at: expires_at.clone() }).expect("serializes")
    );
    std::fs::create_dir_all(state_dir).map_err(|e| OpError::internal(format!("The state directory {} cannot be created: {e}.", state_dir.display())))?;
    let p = path(state_dir);
    let tmp = state_dir.join(format!(".{FILE_NAME}.tmp"));
    write_private(&tmp, body.as_bytes()).map_err(|e| OpError::internal(format!("The setup file {} cannot be written: {e}. Make the state directory writable for the service user.", tmp.display())))?;
    std::fs::rename(&tmp, &p).map_err(|e| OpError::internal(format!("The setup file {} cannot be put in place: {e}.", p.display())))?;
    Ok((secret, expires_at))
}

#[cfg(unix)]
fn write_private(p: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(p);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(p)?;
    f.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(p: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(p, bytes)
}

/// Checks a presented setup secret against the open window.
pub fn check(state_dir: &Path, presented: Option<&str>) -> OpResult<()> {
    let closed = || {
        OpError::gone(
            "setup_window_closed",
            "The setup window is closed. Restart the service, or run `portal-download bootstrap` on the server, and use the new setup secret within the time it names.",
        )
    };
    let Some(presented) = presented.filter(|s| !s.is_empty()) else {
        return Err(OpError::unauthorized(
            "setup_secret_missing",
            "This call needs the setup secret in the header X-Setup-Secret. It is in the setup file in the state directory and in the service log line written at start.",
        ));
    };
    let text = std::fs::read_to_string(path(state_dir)).map_err(|_| closed())?;
    let w: WindowFile = toml::from_str(&text).map_err(|_| closed())?;
    let exp = DateTime::parse_from_rfc3339(&w.expires_at).map_err(|_| closed())?;
    if exp < Utc::now() {
        return Err(closed());
    }
    if !token::equal(&w.secret, presented) {
        return Err(OpError::unauthorized(
            "setup_secret_wrong",
            "The setup secret does not match the open window. Copy it again from the setup file or from the most recent start log line; a restart replaces it.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_window_accepts_its_secret_and_nothing_else() {
        let d = tempfile::tempdir().unwrap();
        let (s, _) = open(d.path(), 5).unwrap();
        check(d.path(), Some(&s)).unwrap();
        assert_eq!(check(d.path(), Some("pds_wrong")).unwrap_err().kind, "setup_secret_wrong");
        assert_eq!(check(d.path(), None).unwrap_err().kind, "setup_secret_missing");
    }

    #[test]
    fn expired_window_is_closed() {
        let d = tempfile::tempdir().unwrap();
        let (s, _) = open(d.path(), -1).unwrap();
        assert_eq!(check(d.path(), Some(&s)).unwrap_err().kind, "setup_window_closed");
    }

    #[test]
    fn no_file_means_closed() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(check(d.path(), Some("x")).unwrap_err().kind, "setup_window_closed");
    }

    #[cfg(unix)]
    #[test]
    fn file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        open(d.path(), 5).unwrap();
        let mode = std::fs::metadata(path(d.path())).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
