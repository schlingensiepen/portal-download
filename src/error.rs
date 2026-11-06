//! The operations' own error vocabulary. Each surface translates it exactly once:
//! the JSON-RPC surface into an error object, the command line into an exit code.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpError {
    /// HTTP status semantics: 400, 401, 403, 404, 409, 410, 500.
    pub code: u16,
    /// Stable machine-readable identifier.
    pub kind: &'static str,
    /// Plain text for humans: what was rejected and the correct next step.
    pub message: String,
}

impl OpError {
    pub fn new(code: u16, kind: &'static str, message: impl Into<String>) -> Self {
        Self { code, kind, message: message.into() }
    }
    pub fn bad(kind: &'static str, message: impl Into<String>) -> Self {
        Self::new(400, kind, message)
    }
    pub fn unauthorized(kind: &'static str, message: impl Into<String>) -> Self {
        Self::new(401, kind, message)
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(403, "right_missing", message)
    }
    pub fn not_found(kind: &'static str, message: impl Into<String>) -> Self {
        Self::new(404, kind, message)
    }
    pub fn conflict(kind: &'static str, message: impl Into<String>) -> Self {
        Self::new(409, kind, message)
    }
    pub fn gone(kind: &'static str, message: impl Into<String>) -> Self {
        Self::new(410, kind, message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(500, "internal", message)
    }
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): {}", self.kind, self.code, self.message)
    }
}

impl From<rusqlite::Error> for OpError {
    fn from(e: rusqlite::Error) -> Self {
        // The detail goes to the operator's log only; callers get no internals.
        eprintln!("portal-download: store error: {e}");
        OpError::internal("The data store reported an error. Retry once; if it persists, the operator finds the detail in the service log.")
    }
}

pub type OpResult<T> = Result<T, OpError>;
