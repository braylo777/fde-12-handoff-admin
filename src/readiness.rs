//! Handoff readiness: can the customer run this without the FDE?
//! Each item has a weight; an item counts only with evidence (a link, a ticket, a test run).
//! Any "blocking" item without evidence means no-go, whatever the score.

use serde_json::{json, Value};

pub const DEFAULT_ITEMS: &[(&str, &str, f64, bool)] = &[
    (
        "runbooks",
        "Runbooks for top 5 incidents, linted and dry-run with customer on-call",
        15.0,
        true,
    ),
    (
        "on_call",
        "Customer on-call rotation owns alerts (paged in the last drill)",
        15.0,
        true,
    ),
    (
        "admin_portal",
        "Customer admin changed a prompt and rolled it back without the FDE",
        15.0,
        true,
    ),
    (
        "access_review",
        "Access review done: FDE and vendor accounts removed or time-boxed",
        10.0,
        true,
    ),
    (
        "evals",
        "Eval suite runs in customer CI on every config change",
        10.0,
        false,
    ),
    (
        "dashboards",
        "Cost, quality and usage dashboards shared with the business owner",
        10.0,
        false,
    ),
    (
        "chaos",
        "Chaos tests passed in the customer VPC",
        10.0,
        false,
    ),
    (
        "training",
        "IT enablement session recorded; 2+ admins trained",
        10.0,
        false,
    ),
    (
        "escalation",
        "Escalation path to the vendor documented with SLAs",
        5.0,
        false,
    ),
];

pub fn score_op(i: &Value) -> Result<Value, String> {
    let ev = i["evidence"]
        .as_object()
        .ok_or("missing `evidence` {item_id: \"link or note\"}")?;
    let mut total = 0.0;
    let mut got = 0.0;
    let mut missing = Vec::new();
    let mut blocking = Vec::new();
    for (id, desc, w, block) in DEFAULT_ITEMS {
        total += w;
        let has = ev
            .get(*id)
            .and_then(Value::as_str)
            .map(|s| s.trim().len() >= 5)
            .unwrap_or(false);
        if has {
            got += w;
        } else {
            missing.push(json!({"item": id, "what": desc, "weight": w, "blocking": block}));
            if *block {
                blocking.push(id.to_string());
            }
        }
    }
    let score = (got / total * 100.0).round();
    let go = blocking.is_empty() && score >= 80.0;
    Ok(
        json!({"score": score, "decision": if go { "GO: hand off" } else { "NO-GO" },
        "blocking_gaps": blocking, "missing": missing,
        "unknown_items": ev.keys().filter(|k| !DEFAULT_ITEMS.iter().any(|d| d.0 == k.as_str())).collect::<Vec<_>>()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocking_gap_means_no_go() {
        let mut ev = serde_json::Map::new();
        for d in DEFAULT_ITEMS.iter().skip(1) {
            ev.insert(d.0.to_string(), json!("https://evidence/link"));
        }
        let r = score_op(&json!({"evidence": ev})).unwrap();
        assert_eq!(r["score"], 85.0);
        assert_eq!(r["decision"], "NO-GO");
        assert_eq!(r["blocking_gaps"], json!(["runbooks"]));
    }
    #[test]
    fn all_evidence_go() {
        let ev: serde_json::Map<String, Value> = DEFAULT_ITEMS
            .iter()
            .map(|d| (d.0.to_string(), json!("ticket INFRA-1234")))
            .collect();
        assert_eq!(
            score_op(&json!({"evidence": ev})).unwrap()["decision"],
            "GO: hand off"
        );
    }
}
