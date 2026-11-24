//! Trace viewer for customer IT: OTLP/JSON in, readable span trees out.
//!
//! Input is the standard OTLP/JSON export (`resourceSpans[].scopeSpans[].spans[]`), the same
//! shape fde-09-llm-telemetry writes and any OpenTelemetry collector can file-export. Output,
//! per trace: a waterfall (indented span tree with offsets and durations), the slowest span,
//! errors, and GenAI totals (model, input / output tokens). Filters let an admin find
//! "errored traces for service X slower than 2 s" without a vendor ticket.

use serde_json::{json, Value};
use std::collections::BTreeMap;

fn attr(attrs: &Value, key: &str) -> Option<Value> {
    attrs.as_array()?.iter().find(|a| a["key"] == key).map(|a| {
        let v = &a["value"];
        if let Some(s) = v["stringValue"].as_str() {
            json!(s)
        } else if let Some(i) = v["intValue"].as_str().and_then(|s| s.parse::<i64>().ok()) {
            json!(i)
        } else if let Some(i) = v["intValue"].as_i64() {
            json!(i)
        } else if let Some(d) = v["doubleValue"].as_f64() {
            json!(d)
        } else if let Some(b) = v["boolValue"].as_bool() {
            json!(b)
        } else {
            v.clone()
        }
    })
}

fn nanos(v: &Value) -> Option<u64> {
    v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_u64())
}

struct Span {
    id: String,
    parent: String,
    name: String,
    service: String,
    start: u64,
    end: u64,
    error: Option<String>,
    model: Option<String>,
    tin: i64,
    tout: i64,
}

pub fn view_op(i: &Value) -> Result<Value, String> {
    let rs = i.get("otlp").ok_or("missing field `otlp`")?["resourceSpans"].as_array().ok_or("otlp.resourceSpans must be a list")?;
    let f = &i["filter"];
    let want_service = f["service"].as_str();
    let only_errors = f["errors_only"].as_bool().unwrap_or(false);
    let min_ms = f["min_duration_ms"].as_f64().unwrap_or(0.0);
    let limit = f["limit"].as_u64().unwrap_or(20) as usize;
    let mut traces: BTreeMap<String, Vec<Span>> = BTreeMap::new();
    let mut bad = 0;
    for r in rs {
        let service = attr(&r["resource"]["attributes"], "service.name")
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "unknown".into());
        for ss in r["scopeSpans"].as_array().into_iter().flatten() {
            for s in ss["spans"].as_array().into_iter().flatten() {
                let (Some(tid), Some(sid), Some(st), Some(en)) = (
                    s["traceId"].as_str(),
                    s["spanId"].as_str(),
                    nanos(&s["startTimeUnixNano"]),
                    nanos(&s["endTimeUnixNano"]),
                ) else {
                    bad += 1;
                    continue;
                };
                let a = &s["attributes"];
                let error = if s["status"]["code"] == 2 || s["status"]["code"] == "STATUS_CODE_ERROR" {
                    Some(
                        s["status"]["message"].as_str().map(String::from)
                            .or_else(|| attr(a, "error.type").and_then(|v| v.as_str().map(String::from)))
                            .unwrap_or_else(|| "error".into()),
                    )
                } else {
                    None
                };
                traces.entry(tid.to_string()).or_default().push(Span {
                    id: sid.into(),
                    parent: s["parentSpanId"].as_str().unwrap_or("").into(),
                    name: s["name"].as_str().unwrap_or("?").into(),
                    service: service.clone(),
                    start: st,
                    end: en.max(st),
                    error,
                    model: attr(a, "gen_ai.request.model").and_then(|v| v.as_str().map(String::from)),
                    tin: attr(a, "gen_ai.usage.input_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
                    tout: attr(a, "gen_ai.usage.output_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
                });
            }
        }
    }
    let total = traces.len();
    let mut out = Vec::new();
    for (tid, mut spans) in traces {
        spans.sort_by_key(|s| s.start);
        let t0 = spans.iter().map(|s| s.start).min().unwrap_or(0);
        let t1 = spans.iter().map(|s| s.end).max().unwrap_or(0);
        let dur_ms = (t1 - t0) as f64 / 1e6;
        let errors: Vec<Value> = spans
            .iter()
            .filter_map(|s| s.error.as_ref().map(|e| json!({"span": s.name, "service": s.service, "error": e})))
            .collect();
        if only_errors && errors.is_empty() {
            continue;
        }
        if dur_ms < min_ms {
            continue;
        }
        if let Some(w) = want_service {
            if !spans.iter().any(|s| s.service == w) {
                continue;
            }
        }
        // Waterfall: depth-first from roots (spans whose parent is absent in this trace).
        let ids: Vec<&str> = spans.iter().map(|s| s.id.as_str()).collect();
        let mut lines = Vec::new();
        let mut stack: Vec<(usize, usize)> = spans
            .iter()
            .enumerate()
            .filter(|(_, s)| s.parent.is_empty() || !ids.contains(&s.parent.as_str()))
            .map(|(k, _)| (k, 0))
            .rev()
            .collect();
        let mut visited = vec![false; spans.len()];
        let mut next_orphan = 0;
        loop {
            // Spans in a parent cycle are never reached from a root; show them as roots.
            if stack.is_empty() {
                while next_orphan < spans.len() && visited[next_orphan] {
                    next_orphan += 1;
                }
                if next_orphan == spans.len() {
                    break;
                }
                stack.push((next_orphan, 0));
            }
            let Some((k, depth)) = stack.pop() else { break };
            if visited[k] || depth > 64 {
                continue;
            }
            visited[k] = true;
            let s = &spans[k];
            lines.push(format!(
                "{}{} [{}] +{:.0}ms {:.0}ms{}{}",
                "  ".repeat(depth),
                s.name,
                s.service,
                (s.start - t0) as f64 / 1e6,
                (s.end - s.start) as f64 / 1e6,
                s.model.as_ref().map(|m| format!(" {m} {}→{} tok", s.tin, s.tout)).unwrap_or_default(),
                s.error.as_ref().map(|e| format!(" ERROR: {e}")).unwrap_or_default()
            ));
            for (c, ch) in spans.iter().enumerate().rev() {
                if ch.parent == s.id && !visited[c] {
                    stack.push((c, depth + 1));
                }
            }
        }
        let slow = spans.iter().max_by_key(|s| s.end - s.start).map(|s| json!({"span": s.name, "ms": (s.end - s.start) as f64 / 1e6}));
        let mut models: Vec<String> = spans.iter().filter_map(|s| s.model.clone()).collect();
        models.sort();
        models.dedup();
        out.push(json!({"trace_id": tid, "duration_ms": dur_ms, "spans": spans.len(),
            "root": lines.first().map(|l| l.split(" [").next().unwrap_or("").to_string()),
            "errors": errors, "slowest_span": slow, "models": models,
            "tokens": {"input": spans.iter().fold(0i64, |a, s| a.saturating_add(s.tin.max(0))), "output": spans.iter().fold(0i64, |a, s| a.saturating_add(s.tout.max(0)))},
            "waterfall": lines}));
    }
    out.sort_by(|a, b| {
        b["errors"].as_array().map(|e| !e.is_empty()).cmp(&a["errors"].as_array().map(|e| !e.is_empty()))
            .then(b["duration_ms"].as_f64().partial_cmp(&a["duration_ms"].as_f64()).unwrap_or(std::cmp::Ordering::Equal))
    });
    let matched = out.len();
    out.truncate(limit);
    Ok(json!({"traces_total": total, "traces_matched": matched, "skipped_spans": bad, "traces": out}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(t: &str, id: &str, parent: &str, name: &str, st: u64, en: u64, err: bool) -> Value {
        let mut s = json!({"traceId": t, "spanId": id, "parentSpanId": parent, "name": name,
            "startTimeUnixNano": st.to_string(), "endTimeUnixNano": en.to_string(), "attributes": []});
        if err {
            s["status"] = json!({"code": 2, "message": "429 rate limited"});
        }
        if name == "chat" {
            s["attributes"] = json!([{"key": "gen_ai.request.model", "value": {"stringValue": "m1"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "100"}},
                {"key": "gen_ai.usage.output_tokens", "value": {"intValue": "20"}}]);
        }
        s
    }
    fn otlp(spans: Vec<Value>) -> Value {
        json!({"resourceSpans": [{"resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "agent"}}]},
            "scopeSpans": [{"spans": spans}]}]})
    }

    #[test]
    fn builds_tree_and_totals() {
        let o = otlp(vec![
            span("t1", "a", "", "handle_ticket", 0, 3_000_000_000, false),
            span("t1", "b", "a", "chat", 100_000_000, 1_100_000_000, false),
            span("t1", "c", "a", "tool.lookup_order", 1_200_000_000, 1_500_000_000, false),
        ]);
        let r = view_op(&json!({"otlp": o})).unwrap();
        let t = &r["traces"][0];
        assert_eq!(t["duration_ms"], 3000.0);
        assert_eq!(t["tokens"]["input"], 100);
        let w: Vec<&str> = t["waterfall"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
        assert!(w[0].starts_with("handle_ticket"));
        assert!(w[1].starts_with("  chat") && w[1].contains("m1 100→20 tok"));
        assert!(w[2].starts_with("  tool.lookup_order"));
    }

    #[test]
    fn filters_errors_and_duration() {
        let o = otlp(vec![
            span("t1", "a", "", "ok", 0, 1_000_000, false),
            span("t2", "x", "", "bad", 0, 5_000_000_000, false),
            span("t2", "y", "x", "chat", 0, 4_000_000_000, true),
        ]);
        let r = view_op(&json!({"otlp": o, "filter": {"errors_only": true}})).unwrap();
        assert_eq!(r["traces_matched"], 1);
        assert_eq!(r["traces"][0]["errors"][0]["error"], "429 rate limited");
        let r = view_op(&json!({"otlp": o, "filter": {"min_duration_ms": 2000}})).unwrap();
        assert_eq!(r["traces_matched"], 1);
        let r = view_op(&json!({"otlp": o, "filter": {"service": "nope"}})).unwrap();
        assert_eq!(r["traces_matched"], 0);
    }

    #[test]
    fn cycles_and_bad_spans_do_not_hang() {
        let o = otlp(vec![
            span("t", "a", "b", "x", 0, 1, false),
            span("t", "b", "a", "y", 0, 1, false),
            json!({"name": "no ids"}),
        ]);
        let r = view_op(&json!({"otlp": o})).unwrap();
        assert_eq!(r["skipped_spans"], 1);
        assert_eq!(r["traces"][0]["waterfall"].as_array().unwrap().len(), 2);
    }
}
