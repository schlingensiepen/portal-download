//! HTTP surface: delivers the static application and the self-description, and passes
//! JSON-RPC calls through to the operations. It translates operation errors exactly once.

use crate::describe;
use crate::error::OpError;
use crate::rpc::{self, Access, Call, Effect};
use crate::store::{self, Principal, Store};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::Router;
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const INDEX_HTML: &[u8] = include_bytes!("../static/index.html");
pub const CONFIG_JSON: &[u8] = include_bytes!("../static/config.json");
/// Languages besides English, the language of the HTML file itself: code and its texts.
pub const LANGUAGES: &[(&str, &[u8])] = &[("de", include_bytes!("../static/i18n/de.json"))];
pub const COOKIE: &str = "pd_device";
/// Browsers cap cookie lifetime at about 400 days; the credential is renewed on every visit.
const COOKIE_MAX_AGE: u64 = 400 * 24 * 3600;

pub struct AppState {
    pub store: Mutex<Store>,
    pub state_dir: PathBuf,
    /// `trust_forwarded_headers` from the configuration.
    pub trust_forwarded: bool,
    csp: String,
}

pub fn app(store: Store, state_dir: PathBuf, trust_forwarded: bool) -> Router {
    let state = Arc::new(AppState { store: Mutex::new(store), state_dir, trust_forwarded, csp: content_security_policy() });
    Router::new()
        .route("/", get(root))
        .route("/index.html", get(index))
        .route("/config.json", get(config_json))
        .route("/i18n/{file}", get(language))
        .route("/describe/usage", get(usage))
        .route("/describe/skill", get(skill))
        .route("/describe/api", get(contract))
        .route("/healthz", get(|| async { ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "running\n") }))
        // The body limit is set per caller in `api`.
        .route("/api/", any(api).layer(axum::extract::DefaultBodyLimit::disable()))
        .route("/backup", get(backup))
        .fallback(fallback)
        .with_state(state)
}

/// The inline script of the single HTML file is allowed by its hash, nothing else runs.
fn content_security_policy() -> String {
    let html = std::str::from_utf8(INDEX_HTML).expect("index.html is UTF-8");
    let mut hashes = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<script>") {
        let after = &rest[start + 8..];
        let end = after.find("</script>").expect("every script element is closed");
        let digest = Sha256::digest(after[..end].as_bytes());
        hashes.push(format!("'sha256-{}'", base64::engine::general_purpose::STANDARD.encode(digest)));
        rest = &after[end..];
    }
    format!(
        "default-src 'none'; script-src {}; style-src 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-src https: http:; form-action 'none'; base-uri 'none'; frame-ancestors 'none'",
        hashes.join(" ")
    )
}

/// Whether the forwarding headers of this request count: from the local reverse proxy
/// always, from every sender when the configuration trusts them.
fn proxied(peer: SocketAddr, trust: bool) -> bool {
    trust || peer.ip().is_loopback()
}

/// The base URL as the caller sees it: scheme and host of the request, as the reverse
/// proxy forwards them when its headers count.
pub fn base_url(h: &HeaderMap, peer: SocketAddr, trust: bool) -> String {
    let get = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let proxied = proxied(peer, trust);
    let fwd = |n: &str| if proxied { forwarded(h, n) } else { None };
    let proto = fwd("x-forwarded-proto").filter(|p| p == "https" || p == "http").unwrap_or_else(|| "http".into());
    let host = fwd("x-forwarded-host").or_else(|| get("host")).unwrap_or_else(|| "localhost".into());
    let host: String = host.chars().filter(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(*c)).collect();
    format!("{proto}://{host}")
}

enum Form {
    Html,
    Markdown,
    Plain,
}

/// Prose defaults to text/plain, also for `*/*` and no Accept; text/markdown only on request.
fn form(h: &HeaderMap, html_allowed: bool) -> Form {
    let accept = h.get(header::ACCEPT).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    if html_allowed && accept.contains("text/html") {
        Form::Html
    } else if accept.contains("text/markdown") {
        Form::Markdown
    } else {
        Form::Plain
    }
}

fn prose(h: &HeaderMap, body: String) -> Response {
    let ct = match form(h, false) {
        Form::Markdown => "text/markdown; charset=utf-8",
        _ => "text/plain; charset=utf-8",
    };
    ([(header::CONTENT_TYPE, ct), (header::VARY, "Accept"), (header::CACHE_CONTROL, "no-store")], body).into_response()
}

fn html(state: &AppState) -> Response {
    let mut r = (StatusCode::OK, INDEX_HTML).into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    h.insert(header::VARY, HeaderValue::from_static("Accept"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_str(&state.csp).expect("ascii"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    r
}

fn lock(st: &AppState) -> std::sync::MutexGuard<'_, Store> {
    // A panic in one request must not take every later request down with it.
    st.store.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn root(State(st): State<Arc<AppState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, h: HeaderMap) -> Response {
    if let Form::Html = form(&h, true) {
        return html(&st);
    }
    let base = base_url(&h, peer, st.trust_forwarded);
    let set_up = lock(&st).has_users().unwrap_or(false);
    prose(&h, if set_up { describe::entry_page(&base) } else { describe::setup_page(&base) })
}

async fn index(State(st): State<Arc<AppState>>) -> Response {
    html(&st)
}

async fn config_json() -> Response {
    ([(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-cache")], CONFIG_JSON).into_response()
}

/// The texts of one language for the browser application, delivered as a file like config.json.
async fn language(axum::extract::Path(file): axum::extract::Path<String>) -> Response {
    match LANGUAGES.iter().find(|(code, _)| file == format!("{code}.json")) {
        Some((_, body)) => (
            [(header::CONTENT_TYPE, "application/json; charset=utf-8"), (header::CACHE_CONTROL, "no-cache"), (header::X_CONTENT_TYPE_OPTIONS, "nosniff")],
            *body,
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "There is no such language file. config.json lists the languages of the browser application.\n",
        )
            .into_response(),
    }
}

async fn usage(State(st): State<Arc<AppState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, h: HeaderMap) -> Response {
    prose(&h, describe::usage(&base_url(&h, peer, st.trust_forwarded)))
}

async fn skill(State(st): State<Arc<AppState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, h: HeaderMap) -> Response {
    prose(&h, describe::skill(&base_url(&h, peer, st.trust_forwarded)))
}

async fn contract(State(st): State<Arc<AppState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, h: HeaderMap) -> Response {
    let body = serde_json::to_string_pretty(&describe::contract(&base_url(&h, peer, st.trust_forwarded))).expect("serializes");
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

async fn fallback(State(st): State<Arc<AppState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, h: HeaderMap) -> Response {
    let base = base_url(&h, peer, st.trust_forwarded);
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!("There is nothing at this address. The entry page at {base}/ links every part of this service; the API is POST {base}/api/.\n"),
    )
        .into_response()
}

// ------------------------------------------------------------------- JSON-RPC

fn rpc_body(status: StatusCode, body: Value, cookie: Option<String>) -> Response {
    let mut r = (status, [(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-store")], body.to_string()).into_response();
    if let Some(c) = cookie {
        r.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&c).expect("cookie is ascii"));
    }
    r
}

fn protocol_error(status: StatusCode, code: i64, message: &str, id: Value) -> Response {
    rpc_body(status, json!({ "jsonrpc": "2.0", "error": { "code": code, "message": message }, "id": id }), None)
}

fn app_error(e: &OpError, id: Value) -> Response {
    rpc_body(StatusCode::OK, json!({ "jsonrpc": "2.0", "error": { "code": e.code, "message": e.message, "data": { "type": e.kind } }, "id": id }), None)
}

fn device_cookie(cred: &str) -> String {
    format!("{COOKIE}={cred}; Path=/; Max-Age={COOKIE_MAX_AGE}; HttpOnly; Secure; SameSite=Lax")
}

fn cookie_value(h: &HeaderMap, name: &str) -> Option<String> {
    h.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|s| s.split(';')).find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name && !v.is_empty()).then(|| v.to_string())
    })
}

/// The last entry of a forwarding header, over all its lines: the one the proxy itself
/// added, whether it replaces the header, appends to it or adds a line.
fn forwarded(h: &HeaderMap, name: &str) -> Option<String> {
    let last = h.get_all(name).iter().filter_map(|v| v.to_str().ok()).next_back()?;
    Some(last.rsplit(',').next()?.trim().to_string()).filter(|s| !s.is_empty())
}

/// The client's address: behind the proxy the last entry of `X-Forwarded-For`.
fn client_address(h: &HeaderMap, peer: SocketAddr, trust: bool) -> String {
    if proxied(peer, trust) {
        if let Some(x) = forwarded(h, "x-forwarded-for") {
            return x;
        }
    }
    peer.ip().to_string()
}

fn bearer(h: &HeaderMap) -> Option<String> {
    h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|s| s.strip_prefix("Bearer ")).map(|s| s.trim().to_string())
}

/// The caller from the device cookie, or else from an agent token; None without either.
fn authenticate(store: &Store, h: &HeaderMap, user_agent: &str, address: &str) -> Option<Result<Principal, OpError>> {
    match (cookie_value(h, COOKIE), bearer(h)) {
        (Some(c), _) => Some(store.authenticate_device(&c, user_agent, address)),
        (None, Some(k)) => Some(store.authenticate_agent(&k)),
        (None, None) => None,
    }
}

/// The backup as a file, for `curl -o` and for the download button of the application.
async fn backup(State(st): State<Arc<AppState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, h: HeaderMap) -> Response {
    let user_agent = h.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let address = client_address(&h, peer, st.trust_forwarded);
    let store = lock(&st);
    let harden = |mut r: Response| {
        let h = r.headers_mut();
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
        h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
        r
    };
    let text_error = |e: &OpError| {
        let status = StatusCode::from_u16(e.code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        harden((status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], format!("{}\n", e.message)).into_response())
    };
    // With the browser's cookie, only the application's own button may start the download;
    // another site navigating here would otherwise drop the backup into the Downloads folder.
    let fetch_site = h.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    if cookie_value(&h, COOKIE).is_some() && fetch_site.is_some_and(|s| s != "same-origin") {
        return text_error(&OpError::forbidden("Start the download from the application's own button under Personen, or use an agent token with curl."));
    }
    let principal = match authenticate(&store, &h, &user_agent, &address) {
        Some(Ok(p)) => p,
        Some(Err(e)) => return text_error(&e),
        None => {
            return text_error(&OpError::unauthorized(
                "credential_missing",
                "The backup needs an agent token granted backup.read in the header Authorization: Bearer, or the browser of a person with the right to administer people.",
            ))
        }
    };
    match store.backup_export(&principal).and_then(|t| store.agent_touch(&principal).map(|_| t)) {
        Ok(text) => {
            let name = format!("attachment; filename=\"portal-download-{}.jsonl\"", store::today().format("%Y-%m-%d"));
            let mut r = harden((StatusCode::OK, [(header::CONTENT_TYPE, "application/jsonl; charset=utf-8")], text).into_response());
            r.headers_mut().insert(header::CONTENT_DISPOSITION, HeaderValue::from_str(&name).expect("ascii"));
            r
        }
        Err(e) => text_error(&e),
    }
}

/// Requests are small, except a backup upload by a person with the right to administer
/// people; anyone else is held to the small limit before the body is read.
const BODY_LIMIT: usize = 1024 * 1024;
const BODY_LIMIT_ADMIN: usize = 32 * 1024 * 1024;

async fn api(State(st): State<Arc<AppState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, method: Method, h: HeaderMap, body: axum::body::Body) -> Response {
    if method != Method::POST {
        return protocol_error(StatusCode::METHOD_NOT_ALLOWED, -32600, "Invalid Request: the API accepts only POST with a JSON-RPC 2.0 body.", Value::Null);
    }
    let limit = match cookie_value(&h, COOKIE) {
        Some(c) => match lock(&st).authenticate_device(&c, h.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or(""), &client_address(&h, peer, st.trust_forwarded)) {
            Ok(Principal::Device { is_admin: true, .. }) => BODY_LIMIT_ADMIN,
            _ => BODY_LIMIT,
        },
        None => BODY_LIMIT,
    };
    let Ok(body): Result<Bytes, _> = axum::body::to_bytes(body, limit).await else {
        return protocol_error(StatusCode::PAYLOAD_TOO_LARGE, -32600, "Invalid Request: the body is too large.", Value::Null);
    };
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return protocol_error(StatusCode::BAD_REQUEST, -32700, "Parse error", Value::Null);
    };
    let Some(obj) = req.as_object() else {
        return protocol_error(StatusCode::OK, -32600, "Invalid Request", Value::Null);
    };
    let id = match obj.get("id") {
        Some(v @ (Value::String(_) | Value::Number(_) | Value::Null)) => v.clone(),
        _ => return protocol_error(StatusCode::OK, -32600, "Invalid Request", Value::Null),
    };
    let (Some("2.0"), Some(name)) = (obj.get("jsonrpc").and_then(Value::as_str), obj.get("method").and_then(Value::as_str)) else {
        return protocol_error(StatusCode::OK, -32600, "Invalid Request", id);
    };
    let params = match obj.get("params") {
        None => json!({}),
        Some(p @ Value::Object(_)) => p.clone(),
        Some(_) => return protocol_error(StatusCode::OK, -32602, "Invalid params", id),
    };
    let Some(m) = rpc::method(name) else {
        return protocol_error(StatusCode::OK, -32601, "Method not found", id);
    };
    let ct = h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !ct.to_ascii_lowercase().starts_with("application/json") {
        return app_error(&OpError::new(415, "content_type", "Send the request with Content-Type: application/json."), id);
    }

    let user_agent = h.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let address = client_address(&h, peer, st.trust_forwarded);
    let setup_secret = h.get("x-setup-secret").and_then(|v| v.to_str().ok()).map(str::to_string);
    let device_cred = cookie_value(&h, COOKIE);
    let store = lock(&st);

    // Establish the caller in one place, before any method runs.
    let auth = authenticate(&store, &h, &user_agent, &address);
    let principal = match auth {
        Some(Ok(p)) => Some(p),
        Some(Err(e)) if !matches!(m.access, Access::Open | Access::Setup) => return app_error(&e, id),
        _ => None,
    };
    let renew = match (&principal, &device_cred) {
        (Some(Principal::Device { .. }), Some(c)) => Some(device_cookie(c)),
        _ => None,
    };

    let call = Call {
        store: &store,
        state_dir: &st.state_dir,
        principal,
        setup_secret: setup_secret.as_deref(),
        user_agent: &user_agent,
        address: &address,
        today: store::today(),
    };
    match rpc::dispatch(name, &params, &call) {
        Ok((result, effect)) => {
            let cookie = match effect {
                Effect::SetDeviceCredential(c) => Some(device_cookie(&c)),
                Effect::ClearDeviceCredential => Some(format!("{COOKIE}=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax")),
                Effect::None => renew,
            };
            rpc_body(StatusCode::OK, json!({ "jsonrpc": "2.0", "result": result, "id": id }), cookie)
        }
        Err(e) => app_error(&e, id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    struct T {
        app: Router,
        dir: tempfile::TempDir,
    }

    fn setup() -> T {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        T { app: app(store, dir.path().to_path_buf(), false), dir }
    }

    async fn send(t: &T, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut req = req;
        req.extensions_mut().insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 5555))));
        let r = t.app.clone().oneshot(req).await.unwrap();
        let (parts, body) = r.into_parts();
        (parts.status, parts.headers, body.collect().await.unwrap().to_bytes().to_vec())
    }

    async fn rpc(t: &T, method: &str, params: Value, extra: &[(&str, &str)]) -> (HeaderMap, Value) {
        let mut b = Request::post("/api/").header("content-type", "application/json").header("host", "pd.intra.test");
        for (k, v) in extra {
            b = b.header(*k, *v);
        }
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params, "id": 1 }).to_string();
        let (s, h, b) = send(t, b.body(Body::from(body)).unwrap()).await;
        assert_eq!(s, StatusCode::OK);
        (h, serde_json::from_slice(&b).unwrap())
    }

    #[tokio::test]
    async fn entry_page_defaults_to_plain_text_and_html_to_browsers() {
        let t = setup();
        for accept in [None, Some("*/*")] {
            let mut b = Request::get("/").header("host", "pd.intra.test");
            if let Some(a) = accept {
                b = b.header("accept", a);
            }
            let (s, h, body) = send(&t, b.body(Body::empty()).unwrap()).await;
            assert_eq!(s, StatusCode::OK);
            assert_eq!(h["content-type"], "text/plain; charset=utf-8");
            assert_eq!(h["vary"], "Accept");
            assert!(String::from_utf8(body).unwrap().contains("nobody has been invited"));
        }
        let (_, h, plain) = send(&t, Request::get("/describe/usage").header("host", "x").body(Body::empty()).unwrap()).await;
        let (_, h2, md) = send(&t, Request::get("/describe/usage").header("host", "x").header("accept", "text/markdown").body(Body::empty()).unwrap()).await;
        assert_eq!(h["content-type"], "text/plain; charset=utf-8");
        assert_eq!(h2["content-type"], "text/markdown; charset=utf-8");
        assert_eq!(plain, md);
        let (_, h, a) = send(&t, Request::get("/").header("accept", "text/html,*/*").body(Body::empty()).unwrap()).await;
        let (_, _, b) = send(&t, Request::get("/").header("accept", "text/html").body(Body::empty()).unwrap()).await;
        assert_eq!(h["content-type"], "text/html; charset=utf-8");
        assert!(h["content-security-policy"].to_str().unwrap().contains("sha256-"));
        assert_eq!(a, b, "two unauthenticated requests yield identical bytes");
    }

    #[tokio::test]
    async fn envelope_errors_follow_json_rpc() {
        let t = setup();
        let (s, _, b) = send(&t, Request::get("/api/").body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
        assert!(serde_json::from_slice::<Value>(&b).unwrap()["error"].is_object());
        let (s, _, b) = send(&t, Request::post("/api/").header("content-type", "application/json").body(Body::from("{nope")).unwrap()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(serde_json::from_slice::<Value>(&b).unwrap()["error"]["code"], -32700);
        let (s, _, b) = send(&t, Request::post("/api/").header("content-type", "application/json").body(Body::from("[]")).unwrap()).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(serde_json::from_slice::<Value>(&b).unwrap()["error"]["code"], -32600);
        let (s, _, b) = send(&t, Request::post("/api/").header("content-type", "application/json").body(Body::from(r#"{"jsonrpc":"2.0","method":"service.status"}"#)).unwrap()).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(serde_json::from_slice::<Value>(&b).unwrap()["error"]["code"], -32600, "notifications are refused");
        let (_, v) = rpc(&t, "nope.nothing", json!({}), &[]).await;
        assert_eq!(v["error"]["code"], -32601);
        let (_, v) = rpc(&t, "job.list", json!({}), &[]).await;
        assert_eq!(v["error"]["code"], 401);
        assert_eq!(v["error"]["data"]["type"], "credential_missing");
    }

    #[tokio::test]
    async fn from_setup_to_first_job() {
        let t = setup();
        let (_, v) = rpc(&t, "service.status", json!({}), &[]).await;
        assert_eq!(v["result"]["setup_required"], true);

        let (secret, _) = crate::bootstrap::open(t.dir.path(), 5).unwrap();
        let (_, v) = rpc(&t, "setup.createAdmin", json!({"name":"Ada"}), &[("x-setup-secret", "wrong")]).await;
        assert_eq!(v["error"]["data"]["type"], "setup_secret_wrong");
        let (_, v) = rpc(&t, "setup.createAdmin", json!({"name":"Ada"}), &[("x-setup-secret", &secret)]).await;
        let tok = v["result"]["invitation_token"].as_str().unwrap().to_string();

        let (h, v) = rpc(&t, "invitation.redeem", json!({"token": tok, "label": "Laptop"}), &[]).await;
        assert_eq!(v["result"]["name"], "Ada");
        let set_cookie = h["set-cookie"].to_str().unwrap().to_string();
        assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("Secure") && set_cookie.contains("SameSite=Lax"));
        let cookie = set_cookie.split(';').next().unwrap().to_string();

        let (h, v) = rpc(&t, "job.create", json!({"title":"Energy","entry_url":"https://portal.example.org","interval_weeks":4}), &[("cookie", &cookie)]).await;
        assert_eq!(v["result"]["title"], "Energy");
        assert!(h.contains_key("set-cookie"), "credential is renewed on every visit");
        let (_, v) = rpc(&t, "job.list", json!({}), &[("cookie", &cookie)]).await;
        assert_eq!(v["result"]["jobs"].as_array().unwrap().len(), 1);

        // Signing out the calling device clears its cookie.
        let (_, v) = rpc(&t, "device.list", json!({}), &[("cookie", &cookie)]).await;
        let me = v["result"]["current_device_id"].as_i64().unwrap();
        let (h, _) = rpc(&t, "device.revoke", json!({"device_id": me}), &[("cookie", &cookie)]).await;
        assert!(h["set-cookie"].to_str().unwrap().contains("Max-Age=0"));
        let (_, v) = rpc(&t, "job.list", json!({}), &[("cookie", &cookie)]).await;
        assert_eq!(v["error"]["data"]["type"], "device_revoked");

        let (_, h, body) = send(&t, Request::get("/").header("host", "pd.intra.test").body(Body::empty()).unwrap()).await;
        assert_eq!(h["content-type"], "text/plain; charset=utf-8");
        assert!(String::from_utf8(body).unwrap().contains("http://pd.intra.test/describe/usage"));
    }

    #[tokio::test]
    async fn base_url_follows_the_proxy() {
        let mut h = HeaderMap::new();
        h.insert("host", "127.0.0.1:8080".parse().unwrap());
        h.insert("x-forwarded-host", "downloads.intra.test".parse().unwrap());
        h.insert("x-forwarded-proto", "https".parse().unwrap());
        let local = SocketAddr::from(([127, 0, 0, 1], 1));
        let remote = SocketAddr::from(([10, 0, 0, 9], 1));
        assert_eq!(base_url(&h, local, false), "https://downloads.intra.test");
        assert_eq!(base_url(&h, remote, false), "http://127.0.0.1:8080", "forwarding headers from elsewhere are ignored");
        assert_eq!(base_url(&h, remote, true), "https://downloads.intra.test", "trusted, they count from every sender");
    }

    #[test]
    fn client_address_follows_the_trusted_proxy() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "198.51.100.1, 192.0.2.7".parse().unwrap());
        let remote = SocketAddr::from(([10, 0, 0, 9], 1));
        assert_eq!(client_address(&h, remote, false), "10.0.0.9");
        assert_eq!(client_address(&h, remote, true), "192.0.2.7", "the entry the proxy added, not what the browser sent");
        h.append("x-forwarded-for", "203.0.113.5".parse().unwrap());
        assert_eq!(client_address(&h, remote, true), "203.0.113.5", "a line the proxy added counts over the browser's");
        let mut h = HeaderMap::new();
        h.insert("host", "portal-download:8080".parse().unwrap());
        h.insert("x-forwarded-host", "evil.example, downloads.intra.test".parse().unwrap());
        h.insert("x-forwarded-proto", "http, https".parse().unwrap());
        assert_eq!(base_url(&h, remote, true), "https://downloads.intra.test", "scheme and host as the proxy added them");
    }

    #[tokio::test]
    async fn backup_download_needs_a_backup_token_and_comes_as_a_file() {
        let t = setup();
        let get = |auth: Option<String>| {
            let mut b = Request::get("/backup");
            if let Some(a) = auth {
                b = b.header(header::AUTHORIZATION, format!("Bearer {a}"));
            }
            b.body(Body::empty()).unwrap()
        };
        let (s, _, b) = send(&t, get(None)).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert!(String::from_utf8(b).unwrap().contains("backup.read"));
        let token = {
            let st = Store::open(t.dir.path()).unwrap();
            let r = st.setup_create_admin("Ada", "").unwrap();
            let (c, _) = st.redeem_invitation(r["invitation_token"].as_str().unwrap(), "Laptop", "", "").unwrap();
            let a = st.authenticate_device(&c, "", "").unwrap();
            let reader = st.agent_create(&a, "NAS nightly backup", "backup.read").unwrap()["token"].as_str().unwrap().to_string();
            let other = st.agent_create(&a, "job reader", "jobs.read").unwrap()["token"].as_str().unwrap().to_string();
            (reader, other, c)
        };
        // With the cookie, another site cannot start the download; the application's own button can.
        for (site, expect) in [("cross-site", StatusCode::FORBIDDEN), ("same-origin", StatusCode::OK)] {
            let req = Request::get("/backup").header(header::COOKIE, format!("{COOKIE}={}", token.2)).header("sec-fetch-site", site).body(Body::empty()).unwrap();
            assert_eq!(send(&t, req).await.0, expect, "{site}");
        }
        let (s, _, _) = send(&t, get(Some(token.1))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, h, b) = send(&t, get(Some(token.0))).await;
        assert_eq!(s, StatusCode::OK);
        assert!(h[header::CONTENT_DISPOSITION].to_str().unwrap().contains(".jsonl"));
        assert_eq!(h[header::CACHE_CONTROL], "no-store");
        let first = String::from_utf8(b).unwrap().lines().next().unwrap().to_string();
        assert!(first.contains("portal-download-backup"));
    }

    #[tokio::test]
    async fn only_administrators_may_send_large_bodies() {
        let t = setup();
        let big = format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"agent.register\",\"params\":{{\"introduction\":\"{}\"}}}}", "x".repeat(2 * 1024 * 1024));
        let (admin, person) = {
            let st = Store::open(t.dir.path()).unwrap();
            let r = st.setup_create_admin("Ada", "").unwrap();
            let (a, _) = st.redeem_invitation(r["invitation_token"].as_str().unwrap(), "Laptop", "", "").unwrap();
            let pa = st.authenticate_device(&a, "", "").unwrap();
            let r = st.user_create(&pa, "Bob", "", false).unwrap();
            let (b, _) = st.redeem_invitation(r["invitation_token"].as_str().unwrap(), "Phone", "", "").unwrap();
            (a, b)
        };
        for (cookie, expect_too_large) in [(None, true), (Some(person), true), (Some(admin), false)] {
            let mut b = Request::post("/api/").header(header::CONTENT_TYPE, "application/json");
            if let Some(c) = cookie {
                b = b.header(header::COOKIE, format!("{COOKIE}={c}"));
            }
            let (s, _, _) = send(&t, b.body(Body::from(big.clone())).unwrap()).await;
            assert_eq!(s == StatusCode::PAYLOAD_TOO_LARGE, expect_too_large);
        }
    }

    #[tokio::test]
    async fn language_files_are_delivered_and_nothing_else() {
        let t = setup();
        let (s, h, b) = send(&t, Request::get("/i18n/de.json").body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::OK);
        assert!(h[header::CONTENT_TYPE].to_str().unwrap().starts_with("application/json"));
        serde_json::from_slice::<Value>(&b).unwrap();
        for bad in ["/i18n/xx.json", "/i18n/..%2Fconfig.json", "/i18n/de.json.bak", "/i18n/en.json"] {
            let (s, _, _) = send(&t, Request::get(bad).body(Body::empty()).unwrap()).await;
            assert_eq!(s, StatusCode::NOT_FOUND, "{bad}");
        }
    }
}
