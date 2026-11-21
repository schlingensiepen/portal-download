//! Runs the scenarios in `contract-tests/` through the JSON-RPC surface: every backend of
//! jobs passes them, this service as well as the local backend of the browser extension.

use crate::rpc::{self, Call};
use crate::store::{self, Principal, Store};
use chrono::Duration;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::Path;

struct Instance {
    store: Store,
    caller: Principal,
}

fn instance() -> Instance {
    let store = Store::open_in_memory().unwrap();
    let v = store.setup_create_admin("Ada", "").unwrap();
    let (cred, _) = store.redeem_invitation(v["invitation_token"].as_str().unwrap(), "Laptop", "browser", "", "").unwrap();
    let caller = store.authenticate_device(&cred, "browser", "", "").unwrap();
    Instance { store, caller }
}

/// Replaces "$name", "$today" and "$today±N" in every string of the value.
fn substitute(v: &Value, saved: &HashMap<String, Value>) -> Value {
    match v {
        Value::String(s) if s == "$any" => v.clone(),
        Value::String(s) if s.starts_with("$today") => {
            let days: i64 = s["$today".len()..].trim_start_matches('+').parse().unwrap_or(0);
            Value::String((store::today() + Duration::days(days)).format("%Y-%m-%d").to_string())
        }
        Value::String(s) if s.starts_with('$') => saved.get(&s[1..]).cloned().unwrap_or_else(|| panic!("nothing saved as {s}")),
        Value::Array(a) => Value::Array(a.iter().map(|x| substitute(x, saved)).collect()),
        Value::Object(o) => {
            let mut m = Map::new();
            for (k, x) in o {
                let k = if let Some(name) = k.strip_prefix('$') { saved.get(name).and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| k.clone()) } else { k.clone() };
                m.insert(k, substitute(x, saved));
            }
            Value::Object(m)
        }
        _ => v.clone(),
    }
}

/// Whether the actual value matches the expected one as the README of the scenarios states.
fn matches(expected: &Value, actual: &Value, at: &str) -> Result<(), String> {
    match (expected, actual) {
        (Value::String(s), _) if s == "$any" => Ok(()),
        (Value::Object(e), Value::Object(a)) => {
            for (k, ev) in e {
                let av = a.get(k).ok_or_else(|| format!("{at}/{k} is missing"))?;
                matches(ev, av, &format!("{at}/{k}"))?;
            }
            Ok(())
        }
        (Value::Array(e), Value::Array(a)) => {
            if e.len() != a.len() {
                return Err(format!("{at} has {} entries, expected {}: {}", a.len(), e.len(), Value::Array(a.clone())));
            }
            for (i, (ev, av)) in e.iter().zip(a).enumerate() {
                matches(ev, av, &format!("{at}/{i}"))?;
            }
            Ok(())
        }
        (e, a) if e == a => Ok(()),
        (e, a) => Err(format!("{at} is {a}, expected {e}")),
    }
}

fn run(file: &Path) {
    let scenario: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    let name = format!("{} ({})", scenario["name"].as_str().unwrap(), file.file_name().unwrap().to_string_lossy());
    let dir = tempfile::tempdir().unwrap();
    let mut instances: HashMap<String, Instance> = HashMap::new();
    let mut saved: HashMap<String, Value> = HashMap::new();
    for (i, step) in scenario["steps"].as_array().unwrap().iter().enumerate() {
        let on = step.get("on").and_then(Value::as_str).unwrap_or("a").to_string();
        let inst = instances.entry(on).or_insert_with(instance);
        let method = step["call"].as_str().unwrap();
        let params = substitute(&step["params"], &saved);
        let password = step.get("password").and_then(Value::as_str);
        let call = Call {
            store: &inst.store,
            state_dir: dir.path(),
            principal: Some(inst.caller.clone()),
            setup_secret: None,
            file_password: password,
            ca_fingerprint: None,
            user_agent: "",
            address: "",
            today: store::today(),
        };
        let outcome = rpc::dispatch(method, &params, &call).map(|(v, _)| v);
        let ctx = format!("{name}, step {} ({method})", i + 1);
        match (step.get("error").and_then(Value::as_str), outcome) {
            (Some(kind), Err(e)) => assert_eq!(e.kind, kind, "{ctx}: {}", e.message),
            (Some(kind), Ok(v)) => panic!("{ctx}: expected the error {kind}, got {v}"),
            (None, Err(e)) => panic!("{ctx}: failed with {}: {}", e.kind, e.message),
            (None, Ok(result)) => {
                if let Some(expect) = step.get("expect") {
                    if let Err(why) = matches(&substitute(expect, &saved), &result, "") {
                        panic!("{ctx}: {why}\nresult: {result}");
                    }
                }
                if let Some(save) = step.get("save").and_then(Value::as_object) {
                    for (k, pointer) in save {
                        let v = result.pointer(pointer.as_str().unwrap()).unwrap_or_else(|| panic!("{ctx}: nothing at {pointer} to save as {k}"));
                        saved.insert(k.clone(), v.clone());
                    }
                }
            }
        }
    }
}

#[test]
fn every_scenario_passes() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("contract-tests");
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    files.sort();
    assert!(files.len() >= 6, "the scenarios are found");
    for f in files {
        run(&f);
    }
}

#[test]
fn the_matcher_fails_where_it_should() {
    assert!(matches(&serde_json::json!({"a": 1}), &serde_json::json!({"a": 1, "b": 2}), "").is_ok());
    assert!(matches(&serde_json::json!({"a": 1}), &serde_json::json!({"a": 2}), "").is_err());
    assert!(matches(&serde_json::json!({"a": [1]}), &serde_json::json!({"a": [1, 2]}), "").is_err());
    assert!(matches(&serde_json::json!({"c": 1}), &serde_json::json!({"a": 1}), "").is_err());
    assert!(matches(&serde_json::json!({"a": "$any"}), &serde_json::json!({"a": null}), "").is_ok());
}
