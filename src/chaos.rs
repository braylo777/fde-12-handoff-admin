//! Chaos testing for an AI deployment inside a customer's VPC.
//!
//! `chaos_plan` turns a component inventory into experiments with expected behaviour,
//! blast-radius limits and abort conditions. `chaos_score` grades observed results.

use serde_json::{json, Value};

fn faults(kind: &str) -> Vec<(&'static str, &'static str)> {
    match kind {
        "llm_provider" => vec![
            ("return HTTP 429 for 10 min", "requests retry with backoff, then fail over to the backup model; no user-facing 5xx"),
            ("add 20 s latency", "client timeout fires at the configured limit; user sees a graceful 'still working' or fallback"),
        ],
        "vector_db" => vec![("stop the vector DB", "answers abstain with 'cannot search documents right now'; no hallucinated citations")],
        "api_service" => vec![
            ("kill 1 of N pods", "traffic shifts to healthy pods; error rate stays under SLO"),
            ("exhaust memory in 1 pod", "pod is OOM-killed and restarted; no request is lost mid-write"),
        ],
        "workflow_engine" => vec![("kill the worker mid-workflow", "workflow resumes from history; side effects happen exactly once")],
        "identity" => vec![("expire the service credential", "calls fail closed with an auth error and an alert; no anonymous fallback")],
        "queue" => vec![("pause consumers for 15 min", "backlog drains after resume; no duplicate processing")],
        "network" => vec![("block egress to the internet", "nothing breaks: the deployment needs no internet (air-gap check)")],
        _ => vec![("stop the component", "dependent features degrade gracefully and alert")],
    }
}

pub fn plan_op(i: &Value) -> Result<Value, String> {
    let comps = i["components"]
        .as_array()
        .ok_or("missing `components` [{name, kind, replicas?}]")?;
    let env = i["environment"].as_str().unwrap_or("staging");
    let mut exps = Vec::new();
    for c in comps {
        let name = c["name"].as_str().unwrap_or("?");
        let kind = c["kind"].as_str().unwrap_or("");
        let replicas = c["replicas"].as_u64().unwrap_or(1);
        for (fault, expect) in faults(kind) {
            if fault.starts_with("kill 1 of N") && replicas < 2 {
                exps.push(json!({"id": format!("E{:02}", exps.len() + 1), "target": name, "fault": "single replica: skip pod-kill test",
                    "expected": "FINDING: no redundancy; one pod loss is an outage", "severity_if_fails": "high", "run": false}));
                continue;
            }
            exps.push(json!({
                "id": format!("E{:02}", exps.len() + 1), "target": name, "fault": fault, "expected": expect,
                "blast_radius": if env == "production" { "1% of traffic, one zone, max 10 min" } else { "full staging, max 30 min" },
                "abort_if": ["error rate > 5% for 2 min", "any data written to the wrong tenant", "customer on-call says stop"],
                "severity_if_fails": if kind == "identity" || kind == "network" { "high" } else { "medium" },
                "run": true
            }));
        }
    }
    Ok(json!({"environment": env, "experiments": exps,
        "preconditions": ["customer change ticket approved", "on-call from customer IT present", "dashboards and alerts open", "rollback tested"]}))
}

pub fn score_op(i: &Value) -> Result<Value, String> {
    let results = i["results"]
        .as_array()
        .ok_or("missing `results` [{id, met_expectation, detected_by_alert, recovery_s, notes}]")?;
    let mut fails = Vec::new();
    let mut pts = 0.0;
    for r in results {
        let met = r["met_expectation"] == true;
        let alerted = r["detected_by_alert"] == true;
        let rec = r["recovery_s"].as_f64().unwrap_or(f64::MAX);
        let slo = i["recovery_slo_s"].as_f64().unwrap_or(300.0);
        let s = (met as u8 as f64) * 0.6
            + (alerted as u8 as f64) * 0.25
            + if rec <= slo { 0.15 } else { 0.0 };
        pts += s;
        if !met || !alerted || rec > slo {
            fails.push(json!({"id": r["id"], "met_expectation": met, "alerted": alerted, "recovery_s": r["recovery_s"],
                "fix": if !met { "behaviour wrong: fix before handoff" } else if !alerted { "silent: add an alert the customer's on-call will see" } else { "recovery too slow: tune timeouts / restart policy" }}));
        }
    }
    let score = if results.is_empty() {
        0.0
    } else {
        pts / results.len() as f64 * 100.0
    };
    Ok(
        json!({"resilience_score": score.round(), "passed": results.len() - fails.len(), "failed": fails.len(), "follow_ups": fails,
        "handoff_ok": fails.iter().all(|f| f["met_expectation"] == true) && score >= 80.0}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn single_replica_is_a_finding() {
        let r = plan_op(
            &json!({"components": [{"name": "api", "kind": "api_service", "replicas": 1}]}),
        )
        .unwrap();
        assert_eq!(r["experiments"][0]["run"], false);
    }
    #[test]
    fn scoring() {
        let r = score_op(&json!({"results": [
            {"id": "E01", "met_expectation": true, "detected_by_alert": true, "recovery_s": 30},
            {"id": "E02", "met_expectation": true, "detected_by_alert": false, "recovery_s": 30}]}))
        .unwrap();
        assert_eq!(r["resilience_score"], 88.0);
        assert_eq!(r["handoff_ok"], true);
        assert_eq!(r["failed"], 1);
    }
}
