//! The service describes itself: entry page, usage description, machine contract and
//! anchor skill, all rendered per request with the base URL the request arrived on.

use crate::rpc::{Access, METHODS};
use serde_json::{json, Value};

pub const ENV_SECRET: &str = "PORTAL_DOWNLOAD_MACHINE_KEY";

/// Entry page while nobody has been invited yet.
pub fn setup_page(base: &str) -> String {
    format!(
        "portal-download at {base}

This is portal-download, an intranet workbench that walks a small circle of people through
recurring downloads of invoices and statements from web portals. It is running but not set
up yet: nobody has been invited.

What to do next:
1. On the server, find the setup secret. It is in the file bootstrap.toml in the service's
   state directory, and in the log line the service wrote at start. A restart, or the command
   `portal-download bootstrap`, writes a fresh one; each is valid for 30 minutes.
2. Open {base}/ in a browser, enter the setup secret and your name. This creates the first
   person with the right to administer people, and opens their invitation on that browser.

An agent can do step 2 with the method setup.createAdmin, described at {base}/describe/api,
sending the setup secret in the header X-Setup-Secret.
"
    )
}

/// Entry page of a set-up instance: about twenty lines, every part with one sentence and one link.
pub fn entry_page(base: &str) -> String {
    format!(
        "portal-download at {base}

Purpose: an intranet workbench for a small, invited circle. It keeps a list of web portals
where invoices and other documents have to be downloaded regularly, puts the most urgent
download job first, opens the portal, offers the stored login for copying, says what to
file where, and moves the next due date when the run is closed as succeeded or failed.

Parts:
- Usage: who may do what, the duties of a caller, how access works.
  {base}/describe/usage
- API contract: every method with parameters, result and an example (OpenRPC, JSON).
  {base}/describe/api
- Anchor skill: save this service as a skill; it holds purpose and links only.
  {base}/describe/skill
- Browser application: open {base}/ in a browser; it fills itself through the API at {base}/api/

Access: by invitation only; there is no self-registration. A machine key for one task is
read from the environment variable {ENV_SECRET}.
"
    )
}

pub fn usage(base: &str) -> String {
    format!(
        "Usage of portal-download at {base}

Answers come as JSON-RPC 2.0 over POST {base}/api/ with Content-Type: application/json.
Every response has HTTP status 200 once the request was readable; an application error
carries error.code (HTTP semantics), error.data.type (stable identifier) and error.message
(what was rejected and what to do next). Follow error.message instead of improvising.

Who may do what
- A person reaches the service through an invitation link. Opening it on a device registers
  that device with its own credential, kept by the browser as a cookie. One link serves all
  of a person's devices until it expires after seven days.
- Every person may work on all download jobs and may sign out their own devices.
- A person with the right to administer people may also invite, re-invite, withdraw
  invitations, revoke other people's devices, deactivate people and grant that right.
- A machine receives a key for exactly one task, never a person's access. Keys are created
  only while the setup window on the server is open; shutting one down is always possible.

Duties of a caller
- Actions that take effect are confirmed with the user before the call; each method that
  takes effect says so in the contract.
- The login of a portal is sealed in the browser with the vault key; the service stores only
  sealed values and cannot read them. Never send a login in plain text.
- A secret is obtained in this order: the environment variable {ENV_SECRET}, then a file
  whose path the user names, used only to pass the value through, and only last a value the
  user types into the conversation, which is then stored at once and never repeated.
- Talk about a machine key by its display string, never by the key.
- Use one anchor per instance; if two instances are known and a request does not say which
  one is meant, ask.

Credentials travel in headers only: the device cookie, Authorization: Bearer for a machine
key, X-Setup-Secret during setup. They never appear in an address or in params.
"
    )
}

pub fn skill(base: &str) -> String {
    format!(
        "---
name: portal-download
description: Download jobs of the portal-download workbench at {base}: which portal downloads are due, and closing them.
---

# portal-download

Purpose: the intranet workbench at {base} that lists recurring portal downloads by urgency.

Base URL: {base}

Load the part a task needs, when it needs it:
- Entry page, start here: {base}/
- Usage and duties of a caller: {base}/describe/usage
- API contract with every method: {base}/describe/api

Secret: environment variable {ENV_SECRET}.
"
    )
}

fn access_text(a: Access) -> &'static str {
    match a {
        Access::Open => "No credential needed.",
        Access::Setup => "Needs the setup secret in the header X-Setup-Secret while the setup window is open.",
        Access::Person => "Needs a person's device credential.",
        Access::Admin => "Needs a person's device credential and the right to administer people.",
        Access::PersonOrMachine => "Needs a person's device credential, or a machine key carrying the task job.list.",
    }
}

/// The machine contract in OpenRPC form, generated from the method table.
pub fn contract(base: &str) -> Value {
    let methods: Vec<Value> = METHODS
        .iter()
        .map(|m| {
            let params: Value = serde_json::from_str(m.params).expect("checked by test");
            let result: Value = serde_json::from_str(m.result).expect("checked by test");
            let param_list: Vec<Value> = params
                .as_object()
                .map(|o| o.iter().map(|(k, v)| json!({ "name": k, "schema": {}, "description": format!("Example value: {v}") })).collect())
                .unwrap_or_default();
            json!({
                "name": m.name,
                "summary": m.summary,
                "description": format!("{} {}", m.description, access_text(m.access)),
                "paramStructure": "by-name",
                "params": param_list,
                "result": { "name": "result", "schema": {} },
                "examples": [{
                    "name": format!("{} example", m.name),
                    "params": params.as_object().map(|o| o.iter().map(|(k, v)| json!({"name": k, "value": v})).collect::<Vec<_>>()).unwrap_or_default(),
                    "result": { "name": "result", "value": result },
                }],
            })
        })
        .collect();
    json!({
        "openrpc": "1.3.2",
        "info": {
            "title": "portal-download",
            "version": env!("CARGO_PKG_VERSION"),
            "description": format!("JSON-RPC 2.0 at POST {base}/api/. Credentials go in headers. The usage description at {base}/describe/usage states who may do what and the duties of a caller."),
        },
        "servers": [{ "name": "this instance", "url": format!("{base}/api/") }],
        "methods": methods,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    fn every_document(base: &str) -> Vec<(&'static str, String)> {
        vec![
            ("setup page", setup_page(base)),
            ("entry page", entry_page(base)),
            ("usage", usage(base)),
            ("skill", skill(base)),
            ("contract", serde_json::to_string_pretty(&contract(base)).unwrap()),
        ]
    }

    /// No hindsight, no internal identifiers, no repository names in any delivered document.
    #[test]
    fn delivered_documents_carry_no_hindsight_or_inside_knowledge() {
        let forbidden = Regex::new(
            r"(?i)\b(removed|formerly|previously|no longer|deprecated|used to|legacy)\b|\b[FW]\d{1,2}\b|\bADR\b|\bdecision \d|\.rs\b|\bsrc/|Cargo\.toml|\bTODO\b|api\.example\.com|localhost",
        )
        .unwrap();
        for (name, doc) in every_document("https://pd.intra.test") {
            if let Some(m) = forbidden.find(&doc) {
                panic!("{name} contains forbidden text: {:?}", m.as_str());
            }
        }
    }

    /// Every URL is rendered with the host of the request.
    #[test]
    fn urls_follow_the_requesting_host() {
        for host in ["https://a.intra.test", "http://10.1.2.3:8080"] {
            for (name, doc) in every_document(host) {
                for url in Regex::new(r"https?://[^\s\x22,)]+").unwrap().find_iter(&doc) {
                    let u = url.as_str();
                    let allowed_foreign = u.starts_with("https://www.telekom.de");
                    assert!(u.starts_with(host) || allowed_foreign, "{name} has a foreign URL {u}");
                }
            }
        }
    }

    /// The usage description states principles; operation behavior lives at the operation.
    #[test]
    fn usage_description_names_no_operation() {
        let u = usage("https://x.test");
        for m in METHODS {
            assert!(!u.contains(m.name), "usage description names the operation {}", m.name);
        }
    }

    /// Every placeholder names the call that produces its value in the same sentence.
    #[test]
    fn placeholders_name_their_source() {
        let ph = Regex::new(r"<[a-z_ ]+>").unwrap();
        let mut docs: Vec<(&str, String)> = every_document("https://x.test").into_iter().filter(|(n, _)| *n != "contract").collect();
        docs.extend(METHODS.iter().map(|m| (m.name, format!("{} {}", m.summary, m.description))));
        for (name, doc) in docs {
            for sentence in doc.split(['.', '\n']) {
                if ph.is_match(sentence) {
                    assert!(
                        sentence.contains("from this response") || sentence.contains("taken from this response"),
                        "{name}: placeholder without its source in: {sentence}"
                    );
                }
            }
        }
    }

    #[test]
    fn entry_page_is_short() {
        assert!(entry_page("https://x.test").lines().count() <= 25);
    }
}
