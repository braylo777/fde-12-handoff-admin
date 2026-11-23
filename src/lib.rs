//! Stage 12 — The Handoff & Self-Serve Admin.
//!
//! The goal of this stage: the customer's IT team runs the system without the FDE.
//!
//! * `runbook` — lint runbooks (owner, verification, rollback, no dangling or looping steps)
//!   and execute them against observed state, following branches, producing a timeline.
//! * `config`  — a versioned prompt / config store: schema validation, secret and model
//!   allow-list checks, diff, approval for risky keys, and rollback to any version.
//! * `chaos`   — generate chaos experiments for a deployment in the customer's VPC, and
//!   score observed results against expected behaviour.
//! * `readiness` — a handoff checklist with evidence, weighted into a go / no-go score.

mod api;
pub mod chaos;
pub mod config;
pub mod readiness;
pub mod runbook;

pub use api::dispatch;
use serde_json::Value;

pub const OPS: &[&str] = &["lint_runbook", "run_runbook", "config_change", "chaos_plan", "chaos_score", "handoff_readiness"];

pub fn handle(op: &str, i: &Value) -> Result<Value, String> {
    match op {
        "lint_runbook" => runbook::lint_op(i),
        "run_runbook" => runbook::run_op(i),
        "config_change" => config::change_op(i),
        "chaos_plan" => chaos::plan_op(i),
        "chaos_score" => chaos::score_op(i),
        "handoff_readiness" => readiness::score_op(i),
        _ => Err(format!("unknown op `{op}`; known: {}", OPS.join(", "))),
    }
}
