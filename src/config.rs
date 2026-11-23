//! Versioned prompt / config store for customer admins.
//!
//! `history` is the list of versions `{version, author, at, reason, config}`. A change either
//! proposes a new config (`set`: partial update) or rolls back to a version. Every change is
//! validated against the schema; risky keys need a named approver; the result includes a diff.

use serde_json::{json, Map, Value};

fn looks_like_secret(s: &str) -> bool {
    let markers = [
        "sk-",
        "sk_live_",
        "AKIA",
        "xoxb-",
        "ghp_",
        "-----BEGIN",
        "Bearer ",
    ];
    markers.iter().any(|m| s.contains(m))
}

pub fn validate(cfg: &Map<String, Value>, schema: &Value) -> Vec<String> {
    let mut errs = Vec::new();
    let fields = schema["fields"].as_object().cloned().unwrap_or_default();
    for (k, rule) in &fields {
        let v = cfg.get(k);
        if rule["required"] == true && v.is_none() {
            errs.push(format!("{k} is required"));
            continue;
        }
        let Some(v) = v else { continue };
        match rule["type"].as_str().unwrap_or("") {
            "string" => match v.as_str() {
                None => errs.push(format!("{k} must be a string")),
                Some(s) => {
                    if let Some(max) = rule["max_len"].as_u64() {
                        if s.chars().count() as u64 > max {
                            errs.push(format!("{k} is {} chars, max {max}", s.chars().count()));
                        }
                    }
                    if let Some(allowed) = rule["one_of"].as_array() {
                        if !allowed.iter().any(|a| a == v) {
                            errs.push(format!(
                                "{k} must be one of {}",
                                Value::Array(allowed.clone())
                            ));
                        }
                    }
                }
            },
            "number" => match v.as_f64() {
                None => errs.push(format!("{k} must be a number")),
                Some(x) => {
                    if rule["min"].as_f64().map(|m| x < m).unwrap_or(false)
                        || rule["max"].as_f64().map(|m| x > m).unwrap_or(false)
                    {
                        errs.push(format!(
                            "{k}={x} outside [{}, {}]",
                            rule["min"], rule["max"]
                        ));
                    }
                }
            },
            "bool" if !v.is_boolean() => errs.push(format!("{k} must be true/false")),
            _ => {}
        }
    }
    for (k, v) in cfg {
        if !fields.contains_key(k) {
            errs.push(format!("{k} is not a known setting"));
        }
        if v.as_str().map(looks_like_secret).unwrap_or(false) {
            errs.push(format!(
                "{k} contains what looks like a credential; use the secret store reference instead"
            ));
        }
    }
    errs
}

pub fn diff(a: &Map<String, Value>, b: &Map<String, Value>) -> Vec<Value> {
    let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .map(|k| json!({"key": k, "from": a.get(k), "to": b.get(k)}))
        .collect()
}

pub fn change_op(i: &Value) -> Result<Value, String> {
    let schema = i.get("schema").ok_or("missing `schema`")?;
    let history = i["history"].as_array().cloned().unwrap_or_default();
    let author = i["author"].as_str().ok_or("missing `author`")?;
    let reason = i["reason"].as_str().unwrap_or("");
    if reason.trim().len() < 10 {
        return Ok(
            json!({"accepted": false, "errors": ["reason must explain the change (at least 10 characters)"]}),
        );
    }
    let current: Map<String, Value> = history
        .last()
        .and_then(|v| v["config"].as_object().cloned())
        .unwrap_or_default();
    let cur_ver = history
        .last()
        .and_then(|v| v["version"].as_u64())
        .unwrap_or(0);
    let proposed: Map<String, Value> = if let Some(to) = i["rollback_to"].as_u64() {
        history
            .iter()
            .find(|v| v["version"] == to)
            .and_then(|v| v["config"].as_object().cloned())
            .ok_or(format!("no version {to}"))?
    } else {
        let mut p = current.clone();
        for (k, v) in i["set"]
            .as_object()
            .ok_or("provide `set` (changes) or `rollback_to`")?
        {
            if v.is_null() {
                p.remove(k);
            } else {
                p.insert(k.clone(), v.clone());
            }
        }
        p
    };
    let errors = validate(&proposed, schema);
    let changes = diff(&current, &proposed);
    if changes.is_empty() {
        return Ok(json!({"accepted": false, "errors": ["no changes"]}));
    }
    let risky: Vec<&str> = schema["risky_keys"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let touched_risky: Vec<String> = changes
        .iter()
        .filter_map(|c| c["key"].as_str())
        .filter(|k| risky.contains(k))
        .map(String::from)
        .collect();
    let approver = i["approved_by"].as_str().unwrap_or("");
    let mut errs = errors;
    if !touched_risky.is_empty() && (approver.is_empty() || approver == author) {
        errs.push(format!(
            "changes to {touched_risky:?} need approval from someone other than the author"
        ));
    }
    if !errs.is_empty() {
        return Ok(
            json!({"accepted": false, "errors": errs, "diff": changes, "current_version": cur_ver}),
        );
    }
    let mut new_hist = history.clone();
    let entry = json!({"version": cur_ver + 1, "author": author, "approved_by": if approver.is_empty() { Value::Null } else { json!(approver) },
        "reason": reason, "rollback_of": i["rollback_to"], "config": proposed});
    new_hist.push(entry.clone());
    Ok(json!({"accepted": true, "new_version": cur_ver + 1, "diff": changes, "history": new_hist}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn schema() -> Value {
        json!({"risky_keys": ["model", "system_prompt"], "fields": {
            "model": {"type": "string", "required": true, "one_of": ["claude-sonnet-5-5", "claude-haiku-5-5"]},
            "temperature": {"type": "number", "min": 0, "max": 1},
            "system_prompt": {"type": "string", "required": true, "max_len": 50},
            "greeting": {"type": "string", "max_len": 40}}})
    }
    fn hist() -> Value {
        json!([{"version": 1, "author": "fde", "reason": "initial", "config": {"model": "claude-haiku-5-5", "temperature": 0.2, "system_prompt": "Be brief."}}])
    }
    #[test]
    fn safe_change_accepted() {
        let r = change_op(&json!({"schema": schema(), "history": hist(), "author": "it-admin", "reason": "friendlier greeting", "set": {"greeting": "Hi!"}})).unwrap();
        assert_eq!(r["accepted"], true);
        assert_eq!(r["new_version"], 2);
    }
    #[test]
    fn risky_change_needs_other_approver() {
        let base = json!({"schema": schema(), "history": hist(), "author": "it-admin", "reason": "use the bigger model", "set": {"model": "claude-sonnet-5-5"}});
        assert_eq!(change_op(&base).unwrap()["accepted"], false);
        let mut ok = base.clone();
        ok["approved_by"] = json!("cx-lead");
        assert_eq!(change_op(&ok).unwrap()["accepted"], true);
    }
    #[test]
    fn rejects_bad_values_and_secrets() {
        let r = change_op(&json!({"schema": schema(), "history": hist(), "author": "a", "reason": "tuning things up",
            "set": {"temperature": 1.7, "greeting": "token sk-abc123", "colour": "red"}})).unwrap();
        assert_eq!(r["errors"].as_array().unwrap().len(), 3);
    }
    #[test]
    fn rollback() {
        let h = change_op(&json!({"schema": schema(), "history": hist(), "author": "a", "reason": "set a greeting", "set": {"greeting": "Yo"}})).unwrap()["history"].clone();
        let r = change_op(&json!({"schema": schema(), "history": h, "author": "a", "reason": "greeting confused users", "rollback_to": 1})).unwrap();
        assert_eq!(r["accepted"], true);
        assert_eq!(r["diff"][0]["key"], "greeting");
        assert_eq!(r["history"][2]["rollback_of"], 1);
    }
}
