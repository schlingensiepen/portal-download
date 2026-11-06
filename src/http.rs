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
pub const COOKIE: &str = "pd_device";
/// Browsers cap cookie lifetime at about 400 days; the credential is renewed on every visit.
const COOKIE_MAX_AGE: u64 = 400 * 24 * 3600;

pub struct AppState {
    pub store: Mutex<Store>,
    pub state_dir: PathBuf,
    csp: String,
}

pub fn app(store: Store, state_dir: PathBuf) -> Router {
    let state = Arc::new(AppState { store: Mutex::new(store), state_dir, csp: content_security_policy() });
    Router::new()
        .route("/", get(root))
        .route("/index.html", get(index))
        .route("/config.json", get(config_json))
        .route("/describe/usage", get(usage))
        .route("/describe/skill", get(skill))
        .route("/describe/api", get(contract))
        .route("/healthz", get(|| async { ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "running\n") }))
        // A backup upload travels in one request.
        .route("/api/", any(api).layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024)))
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

/// The base URL as the caller sees it: scheme and host of the request. The forwarding
/// headers count only when the request comes from the local reverse proxy.
pub fn base_url(h: &HeaderMap, peer: SocketAddr) -> String {
    let get = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).map(|s| s.split(',').next().unwrap_or("").trim().to_string()).filter(|s| !s.is_empty());
    let proxied = peer.ip().is_loopback();
    let fwd = |n: &str| if proxied { get(n) } else { None };
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
    let base = base_url(&h, peer);
    let set_up = lock(&st).has_users().unwrap_or(false);
    prose(&h, if set_up { describe::entry_page(&base) } else { describe::setup_page(&base) })
}

async fn index(State(st): State<Arc<AppState>>) -> Response {
    html(&st)
}

async fn config_json() -> Response {
    ([(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-cache")], CONFIG_JSON).into_response()
}

async fn usage(ConnectInfo(peer): ConnectInfo<SocketAddr>, h: HeaderMap) -> Response {
    prose(&h, describe::usage(&base_url(&h, peer)))
}

async fn skill(ConnectInfo(peer): ConnectInfo<SocketAddr>, h: HeaderMap) -> Response {
    prose(&h, describe::skill(&base_url(&h, peer)))
}

async fn contract(ConnectInfo(peer): ConnectInfo<SocketAddr>, h: HeaderMap) -> Response {
    let body = serde_json::to_string_pretty(&describe::contract(&base_url(&h, peer))).expect("serializes");
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

async fn fallback(ConnectInfo(peer): ConnectInfo<SocketAddr>, h: HeaderMap) -> Response {
    let base = base_url(&h, peer);
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

fn client_address(h: &HeaderMap, peer: SocketAddr) -> String {
    if peer.ip().is_loopback() {
        if let Some(x) = h.get("x-forwarded-for").and_then(|v| v.to_str().ok()).and_then(|s| s.split(',').next()) {
            return x.trim().to_string();
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
    let address = client_address(&h, peer);
    let store = lock(&st);
    let text_error = |e: &OpError| {
        let status = StatusCode::from_u16(e.code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut r = (status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], format!("{}\n", e.message)).into_response();
        r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        r
    };
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
    match store.backup_export(&principal) {
        Ok(text) => {
            let name = format!("attachment; filename=\"portal-download-{}.jsonl\"", store::today().format("%Y-%m-%d"));
            let mut r = (StatusCode::OK, [(header::CONTENT_TYPE, "application/jsonl; charset=utf-8")], text).into_response();
            r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r.headers_mut().insert(header::CONTENT_DISPOSITION, HeaderValue::from_str(&name).expect("ascii"));
            r
        }
        Err(e) => text_error(&e),
    }
}

async fn api(State(st): State<Arc<AppState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, method: Method, h: HeaderMap, body: Bytes) -> Response {
    if method != Method::POST {
        return protocol_error(StatusCode::METHOD_NOT_ALLOWED, -32600, "Invalid Request: the API accepts only POST with a JSON-RPC 2.0 body.", Value::Null);
    }
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
    let address = client_address(&h, peer);
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
        T { app: app(store, dir.path().to_path_buf()), dir }
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
        assert_eq!(base_url(&h, local), "https://downloads.intra.test");
        assert_eq!(base_url(&h, remote), "http://127.0.0.1:8080", "forwarding headers from elsewhere are ignored");
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
            (reader, other)
        };
        let (s, _, _) = send(&t, get(Some(token.1))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, h, b) = send(&t, get(Some(token.0))).await;
        assert_eq!(s, StatusCode::OK);
        assert!(h[header::CONTENT_DISPOSITION].to_str().unwrap().contains(".jsonl"));
        assert_eq!(h[header::CACHE_CONTROL], "no-store");
        let first = String::from_utf8(b).unwrap().lines().next().unwrap().to_string();
        assert!(first.contains("portal-download-backup"));
    }
}
