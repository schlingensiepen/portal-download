//! Static checks of the browser application (decision: building blocks in the HTML, texts
//! translatable). They read static/index.html and the language files as delivered.

use crate::http::{CONFIG_JSON, INDEX_HTML, LANGUAGES};
use regex::Regex;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

fn html() -> &'static str {
    std::str::from_utf8(INDEX_HTML).expect("UTF-8")
}

/// A translatable element holds only text and empty value fields, nothing else.
fn i18n_element() -> Regex {
    Regex::new(r#"data-i18n="([^"]+)"[^>]*>((?:[^<]|<span data-field="\w+"></span>)*)</"#).unwrap()
}

/// The value fields of a text, each as often as it occurs.
fn fields_of(pattern: &str) -> Vec<String> {
    let mut f: Vec<String> = Regex::new(r"\{(\w+)\}").unwrap().captures_iter(pattern).map(|c| c[1].to_string()).collect();
    f.sort();
    f
}

/// Every text key with its English pattern, and every attribute key with its English value.
fn english() -> BTreeMap<String, String> {
    let field = Regex::new(r#"<span data-field="(\w+)"></span>"#).unwrap();
    let mut keys = BTreeMap::new();
    for c in i18n_element().captures_iter(html()) {
        let pattern = field.replace_all(&c[2], "{$1}");
        let pattern = pattern.split_whitespace().collect::<Vec<_>>().join(" ");
        if let Some(old) = keys.insert(c[1].to_string(), pattern.clone()) {
            assert_eq!(old, pattern, "key {} has two different English texts", &c[1]);
        }
    }
    let tag = Regex::new(r"<[a-zA-Z][^>]*>").unwrap();
    for t in tag.find_iter(html()) {
        for a in ["placeholder", "title", "aria-label"] {
            if let Some(k) = Regex::new(&format!(r#"data-i18n-{a}="([^"]+)""#)).unwrap().captures(t.as_str()) {
                let v = Regex::new(&format!(r#"\s{a}="([^"]*)""#)).unwrap().captures(t.as_str()).map(|c| c[1].to_string()).unwrap_or_default();
                if let Some(old) = keys.insert(k[1].to_string(), v.clone()) {
                    assert_eq!(old, v, "key {} has two different English texts", &k[1]);
                }
            }
        }
    }
    keys
}

#[test]
fn every_translatable_element_holds_only_text_and_value_fields() {
    let all = html().matches("data-i18n=\"").count();
    let strict = i18n_element().captures_iter(html()).count();
    assert_eq!(all, strict, "an element with data-i18n contains markup other than text and empty <span data-field> elements");
}

#[test]
fn every_language_file_has_exactly_the_keys_of_the_html_with_the_same_fields() {
    let en = english();
    assert!(en.len() > 100, "the HTML yields only {} keys", en.len());
    for (code, body) in LANGUAGES {
        let dict: BTreeMap<String, Value> = serde_json::from_slice(body).unwrap_or_else(|e| panic!("{code}.json: {e}"));
        let missing: Vec<_> = en.keys().filter(|k| !dict.contains_key(*k)).collect();
        let orphans: Vec<_> = dict.keys().filter(|k| !en.contains_key(*k)).collect();
        assert!(missing.is_empty(), "{code}.json lacks {missing:?}");
        assert!(orphans.is_empty(), "{code}.json has keys the HTML does not use: {orphans:?}");
        for (k, v) in &dict {
            let text = v.as_str().unwrap_or_else(|| panic!("{code}.json: {k} is not a text"));
            assert_eq!(fields_of(text), fields_of(&en[k]), "{code}.json: {k} has other value fields than the English text");
            assert!(!text.trim().is_empty(), "{code}.json: {k} is empty");
            assert!(!text.contains('<') && !text.contains('>'), "{code}.json: {k} contains markup");
        }
    }
}

#[test]
fn config_lists_english_and_every_language_file() {
    let cfg: Value = serde_json::from_slice(CONFIG_JSON).unwrap();
    let listed: BTreeSet<String> = cfg["languages"].as_array().expect("languages").iter().map(|l| l["code"].as_str().unwrap().to_string()).collect();
    let mut expected: BTreeSet<String> = LANGUAGES.iter().map(|(c, _)| c.to_string()).collect();
    expected.insert("en".into());
    assert_eq!(listed, expected);
}

/// No human-readable text in the markup escapes translation.
#[test]
fn no_untranslated_text_in_the_markup() {
    let body = &html()[html().find("<body>").unwrap()..html().find("<script>").unwrap()];
    let no_comments = Regex::new(r"(?s)<!--.*?-->").unwrap().replace_all(body, "");
    let no_i18n = i18n_element().replace_all(&no_comments, "</");
    // Only the technical lines are exempt from translation; translate="no" elsewhere exempts nothing.
    let no_literal = Regex::new(r#"(?s)<span data-msg="line\.\w+" translate="no">(?:[^<]|<span data-field="\w+"></span>)*</span>"#).unwrap().replace_all(&no_i18n, "");
    for t in Regex::new(r"<[a-zA-Z][^>]*>").unwrap().find_iter(&no_literal) {
        for a in ["placeholder", "title", "aria-label"] {
            if let Some(v) = Regex::new(&format!(r#"\s{a}="([^"]*)""#)).unwrap().captures(t.as_str()) {
                let human = v[1].chars().any(char::is_alphabetic) && !v[1].starts_with("http");
                assert!(!human || t.as_str().contains(&format!("data-i18n-{a}=")), "untranslated {a} in {}", t.as_str());
            }
        }
        // Attributes that would show text without a way to translate it.
        for a in ["alt", "label", "value", "content"] {
            if let Some(v) = Regex::new(&format!(r#"\s{a}="([^"]*)""#)).unwrap().captures(t.as_str()) {
                assert!(!v[1].chars().any(char::is_alphabetic), "text in the attribute {a} of {}", t.as_str());
            }
        }
    }
    let text = Regex::new(r"<[^>]*>").unwrap().replace_all(&no_literal, " ");
    let letters: String = text.split_whitespace().filter(|w| w.chars().any(char::is_alphabetic)).collect::<Vec<_>>().join(" ");
    assert!(letters.is_empty(), "text without data-i18n in the markup: {letters}");
}

/// The script holds no text for people; such text lives in the building blocks.
#[test]
fn no_text_for_people_in_the_script() {
    let start = html().find("<script>").unwrap() + 8;
    let script = &html()[start..start + html()[start..].find("</script>").unwrap()];
    let script = Regex::new(r"(?s)/\*.*?\*/").unwrap().replace_all(script, "");
    let script = Regex::new(r"(?m)(^|\s)//\s.*$").unwrap().replace_all(&script, "");
    let literal = Regex::new(r#""((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)'|`((?:[^`\\]|\\.)*)`"#).unwrap();
    let words = Regex::new(r"(?i)[a-z]{2,}\s+[a-z]{2,}").unwrap();
    // The strict-mode directive and a link relation: not texts for people.
    let allowed = ["use strict", "noopener noreferrer"];
    let found: Vec<&str> = literal
        .captures_iter(&script)
        .map(|c| c.get(1).or(c.get(2)).or(c.get(3)).unwrap().as_str())
        .filter(|s| !allowed.contains(s))
        .filter(|s| words.is_match(s) || s.chars().any(|ch| ch.is_alphabetic() && !ch.is_ascii()))
        .collect();
    assert!(found.is_empty(), "text for people in the script: {found:?}");
}

/// Every building block sits in the body before the one script, so the checks above see it.
#[test]
fn the_markup_has_one_script_and_every_block_before_it() {
    assert_eq!(html().matches("<script").count(), 1, "exactly one script element");
    let end = html().find("</script>").unwrap();
    assert!(!html()[end..].contains("<template"), "a building block after the script escapes the checks");
    assert!(!html()[..html().find("<body>").unwrap()].contains("<template"), "a building block in the head escapes the checks");
}

/// The keys the script asks for exist as messages, every message is translatable under its own
/// key, and every plural has both forms.
#[test]
fn every_message_the_script_uses_exists() {
    let start = html().find("<script>").unwrap();
    let script = &html()[start..];
    let msgs: BTreeSet<String> = Regex::new(r#"data-msg="([^"]+)""#).unwrap().captures_iter(html()).map(|c| c[1].to_string()).collect();
    for c in Regex::new(r#"<span data-msg="([^"]+)"([^>]*)>"#).unwrap().captures_iter(html()) {
        let technical = c[2].contains(r#"translate="no""#) && c[1].starts_with("line.");
        assert!(technical || c[2].contains(&format!(r#"data-i18n="{}""#, &c[1])), "message {} is not translatable under its own key", &c[1]);
    }
    for c in Regex::new(r#"\b(msg|msgNode|plural)\(\s*"([^"]+)""#).unwrap().captures_iter(script) {
        let key = &c[2];
        if &c[1] == "plural" {
            for form in ["one", "other"] {
                assert!(msgs.contains(&format!("{key}.{form}")), "plural {key} lacks the form {form}");
            }
        } else {
            assert!(msgs.contains(key), "the script uses the message {key}, which does not exist");
        }
    }
    for c in Regex::new(r#"\b(?:msg|msgNode|stateChip)\(\s*`([a-z_.]+)\.\$\{"#).unwrap().captures_iter(script) {
        assert!(msgs.iter().any(|m| m.starts_with(&format!("{}.", &c[1]))), "no message starts with {}", &c[1]);
    }
    for c in Regex::new(r#"\bstateChip\(\s*"([^"]+)""#).unwrap().captures_iter(script) {
        assert!(msgs.iter().any(|m| m.starts_with(&format!("{}.", &c[1]))), "no message starts with {}", &c[1]);
    }
}

/// Texts reach the page only through building blocks; a string literal handed to an output
/// directly would bypass translation, however short.
#[test]
fn no_literal_text_is_put_into_the_page() {
    let start = html().find("<script>").unwrap();
    let script = &html()[start..];
    let direct = Regex::new(r#"(?:textContent\s*=|\.title\s*=|toast\(|ask\(|alert\(|confirm\(|new Error\()\s*(?:"([^"]*)"|'([^']*)'|`([^`]*)`)"#).unwrap();
    let found: Vec<&str> = direct
        .captures_iter(script)
        .filter(|c| c.get(1).or(c.get(2)).or(c.get(3)).unwrap().as_str().chars().any(char::is_alphabetic))
        .map(|c| c.get(0).unwrap().as_str())
        .collect();
    assert!(found.is_empty(), "literal text put into the page: {found:?}");
}
