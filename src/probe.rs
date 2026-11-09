//! Whether a portal lets itself be shown inside the application: the server fetches the
//! entry page once, follows its redirects and reads the framing headers of the page it
//! lands on. The answer is a suggestion for the job's switch, never a guarantee: a login
//! page reached later may forbid what the entry page allows.

use reqwest::header::{HeaderMap, CONTENT_SECURITY_POLICY, X_FRAME_OPTIONS};
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// true: may be embedded; false: the browser would refuse; None: could not be checked.
    pub embeddable: Option<bool>,
    /// allowed, x_frame_options, frame_ancestors, insecure, status, unreachable.
    pub finding: &'static str,
    pub detail: String,
}

impl Finding {
    fn new(embeddable: Option<bool>, finding: &'static str, detail: impl Into<String>) -> Self {
        Self { embeddable, finding, detail: detail.into() }
    }
    pub fn to_json(&self, entry_url: &str) -> Value {
        json!({ "entry_url": entry_url, "embeddable": self.embeddable, "finding": self.finding, "detail": self.detail })
    }
}

/// The verdict on the headers of the page the frame would show, as the browsers decide it:
/// a frame-ancestors directive takes precedence over X-Frame-Options; of X-Frame-Options
/// only DENY and SAMEORIGIN count, other values are ignored.
pub fn judge(headers: &HeaderMap) -> Finding {
    let mut has_ancestors = false;
    for value in headers.get_all(CONTENT_SECURITY_POLICY) {
        let Ok(value) = value.to_str() else { continue };
        // One header may carry several policies, separated by commas; every one must allow it.
        for policy in value.split(',') {
            // Within a policy the first frame-ancestors directive counts.
            let Some(directive) = policy.split(';').find(|d| {
                d.split_ascii_whitespace().next().is_some_and(|w| w.eq_ignore_ascii_case("frame-ancestors"))
            }) else {
                continue;
            };
            has_ancestors = true;
            let open = directive
                .split_ascii_whitespace()
                .skip(1)
                .any(|s| s == "*" || s.eq_ignore_ascii_case("https:") || s.eq_ignore_ascii_case("http:"));
            if !open {
                return Finding::new(Some(false), "frame_ancestors", directive.trim());
            }
        }
    }
    if !has_ancestors {
        for v in headers.get_all(X_FRAME_OPTIONS).iter().filter_map(|v| v.to_str().ok()) {
            for word in v.split(',').map(str::trim) {
                if word.eq_ignore_ascii_case("DENY") || word.eq_ignore_ascii_case("SAMEORIGIN") {
                    return Finding::new(Some(false), "x_frame_options", word.to_ascii_uppercase());
                }
            }
        }
    }
    Finding::new(Some(true), "allowed", "")
}

/// At most this many checks wait on the network at once, whoever asks.
static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(10))
            .user_agent(concat!("Mozilla/5.0 (compatible; portal-download/", env!("CARGO_PKG_VERSION"), "; embedding check)"))
            .build()
            .expect("the HTTP client builds")
    })
}

/// Fetches the address and judges the page it ends on; the body is never read.
pub async fn inspect(url: &str) -> Finding {
    let _slot = SLOTS.acquire().await.expect("the semaphore stays open");
    match client().get(url).send().await {
        Err(e) => {
            let why = if e.is_timeout() {
                "no answer within 10 seconds"
            } else if e.is_redirect() {
                "too many redirects"
            } else if e.is_connect() {
                "no connection (name, network or certificate)"
            } else {
                "the request failed"
            };
            Finding::new(None, "unreachable", why)
        }
        // A refusal (401, 403, 404) carries the headers a browser would get too; a server error
        // or a gateway may not.
        Ok(r) if r.status().is_server_error() => {
            Finding::new(None, "status", r.status().as_u16().to_string())
        }
        Ok(r) if downgraded(url, r.url().as_str()) => Finding::new(Some(false), "insecure", r.url().as_str()),
        Ok(r) => judge(r.headers()),
    }
}

/// Whether an https address ends on an http page, which the https page may not embed.
fn downgraded(start: &str, end: &str) -> bool {
    let http = |u: &str| u.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("http://"));
    !http(start) && http(end)
}

/// The check for an entry address: an http address is refused inside the https page as
/// mixed content before any header counts.
pub async fn check(url: &str) -> Finding {
    if url.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("http://")) {
        return Finding::new(Some(false), "insecure", "http");
    }
    inspect(url).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use axum::response::{IntoResponse, Redirect};
    use axum::routing::get;
    use axum::Router;

    fn h(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.append(*k, HeaderValue::from_static(v));
        }
        m
    }

    #[test]
    fn judges_the_framing_headers() {
        assert_eq!(judge(&h(&[])).embeddable, Some(true));
        assert_eq!(judge(&h(&[("x-frame-options", "SAMEORIGIN")])), Finding::new(Some(false), "x_frame_options", "SAMEORIGIN"));
        assert_eq!(judge(&h(&[("x-frame-options", "deny")])), Finding::new(Some(false), "x_frame_options", "DENY"));
        assert_eq!(judge(&h(&[("x-frame-options", "ALLOW-FROM https://a.example/")])).embeddable, Some(true), "browsers ignore ALLOW-FROM");
        assert_eq!(judge(&h(&[("x-frame-options", "ALLOWALL")])).embeddable, Some(true));
        assert_eq!(judge(&h(&[("x-frame-options", "ALLOWALL"), ("x-frame-options", "SAMEORIGIN")])).embeddable, Some(false), "every header counts");
        assert_eq!(judge(&h(&[("x-frame-options", "ALLOWALL, DENY")])).embeddable, Some(false), "every value counts");
        assert_eq!(
            judge(&h(&[("content-security-policy", "frame-ancestors *; frame-ancestors 'none'")])).embeddable,
            Some(true),
            "the first directive of a policy counts"
        );
        let fa = judge(&h(&[("content-security-policy", "default-src 'self'; frame-ancestors 'self' https://a.example")]));
        assert_eq!(fa, Finding::new(Some(false), "frame_ancestors", "frame-ancestors 'self' https://a.example"));
        assert_eq!(judge(&h(&[("content-security-policy", "frame-ancestors 'none'")])).embeddable, Some(false));
        assert_eq!(judge(&h(&[("content-security-policy", "Frame-Ancestors *")])).embeddable, Some(true), "a wildcard allows it");
        assert_eq!(judge(&h(&[("content-security-policy", "frame-ancestors https:")])).embeddable, Some(true));
        assert_eq!(
            judge(&h(&[("content-security-policy", "frame-ancestors *"), ("x-frame-options", "DENY")])).embeddable,
            Some(true),
            "frame-ancestors takes precedence over X-Frame-Options"
        );
        assert_eq!(
            judge(&h(&[("content-security-policy", "frame-ancestors *, frame-ancestors 'self'")])).embeddable,
            Some(false),
            "every policy must allow it"
        );
        assert_eq!(
            judge(&h(&[("content-security-policy", "frame-ancestors *"), ("content-security-policy", "frame-ancestors 'none'")])).embeddable,
            Some(false),
            "every header must allow it"
        );
        assert_eq!(judge(&h(&[("content-security-policy-report-only", "frame-ancestors 'none'")])).embeddable, Some(true), "report-only does not block");
        assert_eq!(judge(&h(&[("content-security-policy", "default-src 'self'")])).embeddable, Some(true));
    }

    #[test]
    fn an_https_address_that_ends_on_http_is_insecure() {
        assert!(downgraded("https://a.example/", "http://a.example/login"));
        assert!(!downgraded("https://a.example/", "https://b.example/login"));
        assert!(!downgraded("http://a.example/", "http://a.example/login"), "an http start is refused before the request");
    }

    #[tokio::test]
    async fn fetches_follows_redirects_and_judges_the_last_page() {
        let app = Router::new()
            .route("/open", get(|| async { "portal" }))
            .route("/closed", get(|| async { ([("x-frame-options", "DENY")], "portal").into_response() }))
            .route("/moved", get(|| async { Redirect::temporary("/closed") }))
            .route("/locked", get(|| async { (axum::http::StatusCode::FORBIDDEN, [("x-frame-options", "SAMEORIGIN")], "no").into_response() }))
            .route("/broken", get(|| async { (axum::http::StatusCode::BAD_GATEWAY, "no").into_response() }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = format!("http://{addr}");
        assert_eq!(inspect(&format!("{base}/open")).await.embeddable, Some(true));
        assert_eq!(inspect(&format!("{base}/closed")).await.finding, "x_frame_options");
        assert_eq!(inspect(&format!("{base}/moved")).await.finding, "x_frame_options", "the page after the redirect counts");
        assert_eq!(inspect(&format!("{base}/locked")).await.finding, "x_frame_options", "a refusal is judged by its headers");
        assert_eq!(inspect(&format!("{base}/broken")).await, Finding::new(None, "status", "502"));
        // A port nobody listens on.
        let free = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead = format!("http://{}/", free.local_addr().unwrap());
        drop(free);
        assert_eq!(inspect(&dead).await.finding, "unreachable");
        assert_eq!(check(&format!("{base}/open")).await, Finding::new(Some(false), "insecure", "http"), "http is mixed content");
    }
}
