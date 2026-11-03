# aic — Architecture Design v0.1

> Cross-audit layer for `aid` agent dispatch. CLI-ifies the Cross-Audit Protocol,
> grounded in LDB execution traces, multi-agent debate, and swap-augmented judging.

## 1. Mission

`aic` audits outputs from `aid`-dispatched coding agents before they land. It
turns the manual Cross-Audit Protocol (the one that caught WOOFi fee direction
and router output unit mismatch in v1.18.0) into a scalable, scriptable, CI-grade
pipeline. It is the fourth pillar alongside `aid` (dispatch), `aib` (backlog),
and future `aia` (context).

## 2. Design Principles (grounded in research)

1. **Deterministic > LLM judgment.** Cheap deterministic checks gate expensive
   LLM ones. A compile fail short-circuits before cross-LLM ever fires.
   *(Aider, Semgrep, Agentless — all converge here.)*
2. **External, never self-reflective.** `aic` runs in a separate process. It
   never asks `aid` to audit its own output — self-reflection collapses on hard
   problems *(Reflexion limits, SycEval persistence)*.
3. **Reference-backed critiques only.** Every fail verdict must cite executable
   evidence: test stderr, compile output, on-chain query, or named rule from
   `.aic/rules/`. No "this looks sketchy to me" *(reference-free judging leads
   to 58% sycophancy per SycEval)*.
4. **Swap-augmented cross-LLM votes.** When N models vote on a diff, positions
   must be swapped on half the runs to null position bias; verdicts normalized
   for verbosity *(AlpacaEval 2.0 LC win rate, JudgeLM)*.
5. **Layered cost.** Audit cost scales with risk. Trivial commits hit L0-L1
   only; security-critical diffs fire the full pipeline. Budget-aware.
6. **Config-as-code.** Adversarial checklists live in `.aic/rules/*.md`,
   versioned with the code they guard *(Continue.dev, PR-Agent pattern)*.

## 3. Verification Pipeline

Layered and cost-ordered. Each layer can short-circuit on fail.

```
aid task output (diff + artifacts + event log)
       │
       ▼
 ┌────────────────────────────────────────┐
 │ L0 Static (deterministic, <5s)         │
 │  - cargo check / tsc / biome / ruff    │
 │  - clippy / eslint                     │
 │  - secret scan (gitleaks)              │
 │  - verify-addresses on address diffs   │
 └────────────────────────────────────────┘
       │ pass
       ▼
 ┌────────────────────────────────────────┐
 │ L1 Test (deterministic, 10-120s)       │
 │  - project-configured verify cmd       │
 │  - targeted test subset (diff-scoped)  │
 │  - fork-based replay (for bots)        │
 └────────────────────────────────────────┘
       │ pass
       ▼
 ┌────────────────────────────────────────┐
 │ L2 Checklist (cheap LLM, ~5s)          │
 │  - .aic/rules/*.md evaluated           │
 │  - configured aid agent (codex)         │
 │  - structured JSON verdict per rule    │
 └────────────────────────────────────────┘
       │ pass
       ▼
 ┌────────────────────────────────────────┐
 │ L3 Cross-LLM swap review (~30s)        │
 │  - N models, positions swapped         │
 │  - adversarial "find the bug" prompts  │
 │  - verdict aggregator with bias norm   │
 └────────────────────────────────────────┘
       │ pass
       ▼
 ┌────────────────────────────────────────┐
 │ L4 Executable evidence (~10s, opt-in)  │
 │  - crpc on-chain calls for claims      │
 │  - custom .aic/checks/*.sh scripts     │
 │  - LDB-style trace verification        │
 └────────────────────────────────────────┘
       │
       ▼
    Verdict → report → aib hook
```

**Fail semantics**: any layer can emit `FailBlocking` (hard stop),
`FailAdvisory` (warn but continue), or `Pass`. Short-circuit on blocking.

## 4. Core Data Types

```rust
struct AuditRequest {
    task_id: Option<TaskId>,      // aid task, or None for standalone
    diff: UnifiedDiff,
    artifacts: Vec<PathBuf>,
    context: AuditContext,         // spec, related files, claim to verify
    skip_layers: Vec<Layer>,
    budget_usd: Option<f64>,
}

enum Layer { L0Static, L1Test, L2Checklist, L3CrossLlm, L4Evidence }

struct AuditResult {
    verdict: Verdict,
    layer_results: Vec<LayerResult>,
    evidence: Vec<Evidence>,
    total_cost_usd: f64,
    duration_ms: u64,
}

enum Verdict {
    Pass,
    FailBlocking(Reason),
    FailAdvisory(Reason),
    Inconclusive,
}

struct Evidence {
    layer: Layer,
    signal: SignalKind,          // Compile, Test, Checklist, LlmVote, Crpc, Script
    passed: bool,
    detail: String,              // stderr, vote text, tx hash
    cite: Option<String>,        // "cargo check" / "rule: no-unsafe" / "crpc:base:0xabc"
    cost_usd: f64,
}
```

All artifacts persist to `.aic/audits/<task-id>/` for later re-examination.

## 5. Commands (MVP scope)

| Command | Purpose |
|---------|---------|
| `aic init` | Scaffold `.aic/` dir with sample config + rules |
| `aic audit <task-id>` | Post-hoc audit of completed `aid` task |
| `aic audit --diff <path>` | Audit a standalone diff file |
| `aic report <task-id>` | Render markdown report from stored audit |
| `aic checklist list` | Show loaded rules |

**Deferred to v0.2+**:
- `aic audit --watch <task-id>` — trajectory scoring during `aid` run
- `aic checklist gen` — LLM-generate draft rules from recent diffs
- `aic verify <claim>` — single-claim verification (fact-oracle CLI-ification)

## 6. Config schema (`.aic/config.toml`)

L2 `model` and L3 `models` are aid agent names. Omitted L0/L1 `commands` use
Cargo defaults only when the repository root contains `Cargo.toml`; otherwise
they skip commands. Explicit lists, including `[]`, always win. `aic init`
writes commands for the current repository.

Judge instructions and diff content are attached whole with `aid run --context`.
The positional prompt is fixed to avoid aid report mode. Judges may read that
context file only. Verdicts come from a single JSON object in the final answer
block after the last `---` line, optionally wrapped in a code fence. Prose and
quoted JSON fixtures cannot supply a verdict.

```toml
[layers.l0]
commands = ["cargo check", "cargo clippy -- -D warnings"]
secret_scan = true
address_verify = true

[layers.l1]
commands = ["cargo test --lib"]
max_duration_secs = 120

[layers.l2]
enabled = true
model = "codex" # aid agent name
rules_dir = ".aic/rules"

[layers.l3]
enabled = true
models = ["codex"] # aid agent names
swap = true
consensus_threshold = 1  # one agreeing agent

[layers.l4]
scripts = [".aic/checks/*.sh"]

[integration]
aid_auto_audit = false    # opt-in per task
aib_auto_block = true     # failing audit → blocks board item
budget_usd_per_audit = 5.0
```

## 7. Adversarial Checklist Format (`.aic/rules/*.md`)

Each rule file is a markdown document with frontmatter:

```markdown
---
id: no-fee-direction-swap
severity: blocking
scope: ["dex/**/*.rs"]
layer: l2
---

# Fee direction must be preserved on cross-swap paths

When the diff modifies a DEX integration's fee calculation, verify:

1. Fee is applied on input (not output) for exact-in swaps
2. Fee is applied on output (not input) for exact-out swaps
3. For cross-chain / cross-pool routes, the fee direction matches the
   quote direction at every hop

Reference: `audits/v1.18.0-woofi-fee-direction.md`

Ask the model: "Trace the fee application through every hop in this diff.
At each hop, state: (1) which side (input/output) the fee applies to,
(2) whether this matches the quote direction, (3) evidence from the code."

Pass if all hops are consistent. Fail if any hop applies fee to the wrong side.
```

Rules are loaded at audit time and converted into structured prompts.

## 8. Integration with existing tools

| Tool | Integration |
|------|-------------|
| `aid` | `aid run ... --audit` flag auto-dispatches `aic audit` post-run. `aid show <task-id>` surfaces audit verdict in the summary. |
| `aib` | `aic` reads current board item from `aid` task metadata. On `FailBlocking`, transitions item to `blocked` with audit report as comment. On `Pass`, transitions to `ready-for-review`. |
| `crpc` | L4 calls `crpc` subprocess for on-chain claim verification (contract addresses, storage slots, call results). |
| `verify-addresses` | L0 invokes `~/.claude/tools/verify-addresses.sh` on any diff touching address constants. |
| `ai-summary` | L2 checklist evaluator uses `ai-summary` to compress long diff context when generating rule prompts. |

## 9. Task Breakdown (MVP v0.1)

14 tasks, ~2 weeks agent work, can parallelize most. Grouped by dependency.

### Foundation (T1-T3 — sequential, then parallel)

- **T1 — aic-001** Crate scaffold + clap CLI skeleton + `.aic/config.toml` loader
- **T2 — aic-002** Core data types: `AuditRequest`, `AuditResult`, `Evidence`, `Verdict`, `Layer`
- **T3 — aic-003** Rule file parser (`.aic/rules/*.md` → `Rule` struct with frontmatter)

### Verifier layers (parallel after T2)

- **T4 — aic-004** L0 static layer runner: execute configured commands, capture stderr as `Evidence`, short-circuit logic
- **T5 — aic-005** L1 test layer runner: project-scoped verify commands with timeout + budget
- **T6 — aic-006** L2 checklist evaluator: load rules, build structured prompt, call LLM via `aid run <agent>`, parse JSON verdict
- **T7 — aic-007** L3 swap-augmented cross-LLM voter: dispatch N agents in parallel, swap positions on half, aggregate with verbosity normalization
- **T8 — aic-008** Evidence aggregator + verdict decision logic (how layer results combine into final `Verdict`)

### Integration (depends on T4-T8)

- **T9 — aic-009** `aic audit <task-id>` end-to-end wiring: read `aid show <task-id>` output, run pipeline, persist to `.aic/audits/`, emit report
- **T10 — aic-010** `aid run --audit` flag + post-run hook that dispatches `aic audit`
- **T11 — aic-011** `aib` integration: on fail, transition board item to blocked with audit report
- **T12 — aic-012** `aic init` scaffolding + one sample rule (e.g. `no-unwrap.md`)

### Dogfood + polish (depends on T9-T12)

- **T13 — aic-013** Replay audit on 3 recent SR bug fixes; compare verdict to manual audit notes; tune rules + prompts
- **T14 — aic-014** README + `.aic/` example + end-to-end quickstart doc

## 10. Out-of-scope for MVP (v0.2+)

- L4 `crpc` / `verify-addresses` integration (useful but blockchain-specific; land it as a plugin after MVP is solid)
- Trajectory critic (`aic audit --watch`) — requires `aid` event-stream subscription API, not ready
- Prometheus 2 / local evaluator model support
- `aic checklist gen` — LLM-generated draft rules from diffs
- `aic verify <claim>` — single-claim mode (fact-oracle CLI-ification)
- IDE / editor integrations

## 11. Success metrics (dogfood)

- **Recall**: catches ≥80% of bugs found by manual cross-audit in past 3 months
- **Precision**: ≤20% false-positive rate (noisy audits waste engineer attention)
- **Latency**: full pipeline <90s for typical SR-sized diff
- **Cost**: <$2 per audit at default config

Measured on T13 dogfood runs.

## 12. Risks

1. **LLM judge bias** — SycEval found 58% sycophancy. Mitigation: swap-augmentation in L3, structured JSON output, reference-backed only.
2. **False positives erode trust** — if first 10 audits are noisy, nobody will use it. Mitigation: T13 tuning pass before release, `FailAdvisory` vs `FailBlocking` distinction.
3. **Prompt injection via diff content** — malicious or accidental prompts in code comments could subvert L2/L3. Mitigation: sanitize diff text, delimit clearly, never execute instructions from diff content.
4. **Cost overrun** — runaway cross-LLM votes. Mitigation: `budget_usd_per_audit` hard cap, early short-circuit.
