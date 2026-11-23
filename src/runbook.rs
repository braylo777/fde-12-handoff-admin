//! Executable runbooks.
//!
//! A step: `{id, title, owner, action, check: {metric, op, value}, on_pass, on_fail, rollback?}`.
//! `on_pass` / `on_fail` name the next step id, or "done" / "escalate". Execution evaluates each
//! check against `observations` (what monitoring shows now) and walks the branches.
//! Actions are recorded, not executed: the operator (or an automation hook) runs them.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

const TERMINAL: [&str; 2] = ["done", "escalate"];

pub fn lint(rb: &Value) -> Vec<Value> {
    let mut f = Vec::new();
    let steps = rb["steps"].as_array().cloned().unwrap_or_default();
    if steps.is_empty() {
        f.push(json!({"step": null, "issue": "runbook has no steps"}));
        return f;
    }
    let ids: Vec<&str> = steps.iter().filter_map(|s| s["id"].as_str()).collect();
    let mut seen = HashSet::new();
    for s in &steps {
        let id = s["id"].as_str().unwrap_or("?");
        if !seen.insert(id) {
            f.push(json!({"step": id, "issue": "duplicate step id"}));
        }
        if s["owner"].as_str().unwrap_or("").is_empty() {
            f.push(json!({"step": id, "issue": "no owner (who runs this at 3 a.m.?)"}));
        }
        if s["check"].is_null() {
            f.push(
                json!({"step": id, "issue": "no verification check: how do we know it worked?"}),
            );
        }
        let mutates = s["action"]
            .as_str()
            .map(|a| {
                [
                    "restart", "rollback", "scale", "deploy", "delete", "failover", "rotate",
                ]
                .iter()
                .any(|k| a.to_lowercase().contains(k))
            })
            .unwrap_or(false);
        if mutates && s["rollback"].is_null() {
            f.push(json!({"step": id, "issue": "changes the system but has no rollback"}));
        }
        for k in ["on_pass", "on_fail"] {
            let t = s[k].as_str().unwrap_or("");
            if t.is_empty() {
                f.push(json!({"step": id, "issue": format!("missing {k}")}));
            } else if !TERMINAL.contains(&t) && !ids.contains(&t) {
                f.push(json!({"step": id, "issue": format!("{k} points to unknown step `{t}`")}));
            }
        }
    }
    // Reachability from the first step.
    let next: HashMap<&str, Vec<&str>> = steps
        .iter()
        .map(|s| {
            (
                s["id"].as_str().unwrap_or(""),
                vec![
                    s["on_pass"].as_str().unwrap_or(""),
                    s["on_fail"].as_str().unwrap_or(""),
                ],
            )
        })
        .collect();
    let mut reach = HashSet::new();
    let mut stack = vec![ids[0]];
    while let Some(n) = stack.pop() {
        if reach.insert(n) {
            for m in next.get(n).into_iter().flatten() {
                if ids.contains(m) {
                    stack.push(m);
                }
            }
        }
    }
    for id in &ids {
        if !reach.contains(id) {
            f.push(json!({"step": id, "issue": "unreachable from the first step"}));
        }
    }
    f
}

fn check(c: &Value, obs: &Value) -> Result<(bool, Value), String> {
    let metric = c["metric"].as_str().ok_or("check.metric missing")?;
    let actual = obs
        .get(metric)
        .cloned()
        .ok_or(format!("no observation for `{metric}`"))?;
    let want = &c["value"];
    let ok = match c["op"].as_str().unwrap_or("==") {
        "==" => &actual == want,
        "!=" => &actual != want,
        "<" => actual.as_f64() < want.as_f64(),
        "<=" => actual.as_f64() <= want.as_f64(),
        ">" => actual.as_f64() > want.as_f64(),
        ">=" => actual.as_f64() >= want.as_f64(),
        o => return Err(format!("unknown operator {o}")),
    };
    Ok((ok, actual))
}

pub fn lint_op(i: &Value) -> Result<Value, String> {
    let f = lint(i.get("runbook").ok_or("missing `runbook`")?);
    Ok(json!({"ok": f.is_empty(), "findings": f}))
}

pub fn run_op(i: &Value) -> Result<Value, String> {
    let rb = i.get("runbook").ok_or("missing `runbook`")?;
    let problems = lint(rb);
    if !problems.is_empty() {
        return Ok(
            json!({"status": "refused", "reason": "runbook failed lint; fix it before running", "findings": problems}),
        );
    }
    let obs = i.get("observations").ok_or("missing `observations`")?;
    let steps = rb["steps"].as_array().unwrap();
    let mut cur = steps[0]["id"].as_str().unwrap().to_string();
    let mut timeline = Vec::new();
    let mut visits: HashMap<String, u32> = HashMap::new();
    let mut actions = Vec::new();
    while !TERMINAL.contains(&cur.as_str()) {
        let v = visits.entry(cur.clone()).or_default();
        *v += 1;
        if *v > 3 {
            timeline
                .push(json!({"step": cur, "result": "loop guard: visited 3 times, escalating"}));
            cur = "escalate".into();
            break;
        }
        let s = steps.iter().find(|s| s["id"] == cur.as_str()).unwrap();
        if let Some(a) = s["action"].as_str() {
            actions.push(
                json!({"step": cur, "owner": s["owner"], "action": a, "rollback": s["rollback"]}),
            );
        }
        let (ok, actual) = check(&s["check"], obs)?;
        let nxt = s[if ok { "on_pass" } else { "on_fail" }]
            .as_str()
            .unwrap()
            .to_string();
        timeline.push(json!({"step": cur, "title": s["title"], "check": s["check"], "observed": actual, "passed": ok, "next": nxt}));
        cur = nxt;
    }
    Ok(json!({"status": cur, "timeline": timeline, "actions_to_run": actions}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rb() -> Value {
        json!({"steps": [
            {"id": "s1", "title": "Is the LLM provider erroring?", "owner": "on-call", "check": {"metric": "provider_5xx_rate", "op": ">", "value": 0.05}, "on_pass": "s2", "on_fail": "s3"},
            {"id": "s2", "title": "Fail over to backup model", "owner": "on-call", "action": "failover to backup endpoint", "rollback": "switch primary back", "check": {"metric": "backup_healthy", "op": "==", "value": true}, "on_pass": "done", "on_fail": "escalate"},
            {"id": "s3", "title": "Check vector DB latency", "owner": "platform", "check": {"metric": "vector_p95_ms", "op": "<", "value": 300}, "on_pass": "done", "on_fail": "escalate"}]})
    }
    #[test]
    fn clean_runbook_lints() {
        assert!(lint(&rb()).is_empty());
    }
    #[test]
    fn finds_problems() {
        let mut r = rb();
        r["steps"][1]["rollback"] = Value::Null;
        r["steps"][2]["on_fail"] = json!("s9");
        r["steps"][0]["owner"] = json!("");
        assert_eq!(lint(&r).len(), 3);
    }
    #[test]
    fn executes_branch() {
        let r = run_op(&json!({"runbook": rb(), "observations": {"provider_5xx_rate": 0.2, "backup_healthy": true}})).unwrap();
        assert_eq!(r["status"], "done");
        assert_eq!(r["actions_to_run"][0]["step"], "s2");
    }
    #[test]
    fn missing_observation_is_error() {
        assert!(run_op(&json!({"runbook": rb(), "observations": {}})).is_err());
    }
    #[test]
    fn loop_guard() {
        let r = json!({"steps": [{"id": "a", "owner": "x", "check": {"metric": "m", "op": "==", "value": 1}, "on_pass": "a", "on_fail": "done"}]});
        let out = run_op(&json!({"runbook": r, "observations": {"m": 1}})).unwrap();
        assert_eq!(out["status"], "escalate");
    }
}
