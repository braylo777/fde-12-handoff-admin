# fde-12-handoff-admin

**Forward Deployed Engineering — Stage 12 of 12: The Handoff & Self-Serve Admin**

Make the FDE unnecessary: runbooks that run, a prompt/config portal with guardrails and rollback, chaos tests in the customer's VPC, and a go / no-go score for handoff.

**Live demo:** https://fde-12-handoff-admin.braylo777.workers.dev — the Rust core runs in your browser as WebAssembly. No server, no tracking.

## Why this matters

A deployment is not done when it works; it is done when the customer's IT team can run it at 3 a.m. without calling you. That needs runbooks that are tested, a safe way for admins to change prompts and settings (with validation, approvals and one-click rollback), proof the system degrades gracefully when parts fail, and an honest checklist that says whether the handoff can happen.

## What it does

- Runbook lint: owner, verification check, rollback for mutating actions, dangling and unreachable steps
- Runbook execution against live observations: branch walk, loop guard, timeline, list of actions for the operator
- Config store: schema validation (types, ranges, allow-lists, length), credential detection, diff, two-person approval for risky keys, rollback, full history
- Chaos plan per component kind (LLM 429 / latency, vector DB down, pod kill, mid-workflow crash, expired credential, egress block) with blast radius and abort rules
- Chaos scoring: behaviour, alerting and recovery time; follow-up per failure
- Handoff readiness: weighted evidence checklist with blocking items

## What it does not do (yet)

- Runbook actions are listed for the operator, not executed (no kubectl or cloud API calls)
- No chaos injection tooling (use Chaos Mesh, Litmus or AWS FIS to run the plan)
- The config store is a pure function over a version list; persist it in your DB or git

## Operations

Every operation takes JSON in and gives JSON out. The CLI, the tests and the demo page all use the same entry point.

| Operation | What it does |
|---|---|
| `run_runbook` | Answers are failing. The provider is fine and retrieval is fast, but a config change went out an hour ago: the runbook leads to a rollback. Change the observations to walk other branches. |
| `lint_runbook` | The same runbook after careless edits: a restart with no rollback, a branch to a step that does not exist, an orphaned step and a step with no owner. |
| `config_change` | A customer admin switches the model and edits the prompt. Both are risky keys, so a second person must approve. Remove approved_by to see it refused, or set temperature to 1.5. |
| `chaos_plan` | Experiments for each component in the customer VPC. The single-replica ingest worker is reported as a finding instead of a test. |
| `chaos_score` | Results from the game day. The vector DB outage was handled but no alert fired; the credential test failed open. Handoff is blocked until both are fixed. |
| `handoff_readiness` | Evidence collected so far. The score is high, but no one outside the FDE has used the admin portal yet, which blocks the handoff. |

## Run it

You need Rust (stable). No Node, no npm.

```bash
git clone https://github.com/braylo-ai/fde-12-handoff-admin
cd fde-12-handoff-admin
cargo test
cargo run -- ops
```

Try the first demo example from the command line:

```bash
python3 -c 'import json;print(json.dumps(json.load(open("web/examples.json"))["ops"][0]["input"]))' | cargo run -q -- run_runbook
```

## Build and deploy the demo

```bash
./build.sh    # tests, then compiles the core to web/app.wasm
./deploy.sh   # serves web/ as a Cloudflare Workers static-assets site (needs a Cloudflare login)
```

`web/` is a plain static site (`index.html`, `app.js`, `app.wasm`). It has no build step, so any static host works.

## Layout

```
src/lib.rs        operation list and router
src/api.rs        JSON entry point + the 3 WASM exports (alloc, dealloc, run)
src/main.rs       CLI
src/*.rs          one module per capability, each with unit tests
tests/            integration tests (every demo example must pass)
web/              demo page; examples.json drives both the page and this README
```

## Design choices

- **Rust, few dependencies.** Only `serde`, `serde_json`, `sha2` and `hmac`, pinned to exact versions with a committed `Cargo.lock`.
- **Deterministic.** Time, randomness and keys are inputs, so every result can be replayed in a test or an audit.
- **Fail closed.** Unknown input, missing fields or an unmatched policy gives an error or a denial, never a silent pass.

## Part of a series

This is one of 12 repos that work through a public Forward Deployed Engineering roadmap ([source post](https://x.com/suraj_sharma14/status/2124827808535048566)). All 12: [github.com/braylo-ai?q=fde-](https://github.com/braylo-ai?tab=repositories&q=fde-).

## License

MIT — see [LICENSE](LICENSE). Built by [Braylo SI](https://braylo.si).
