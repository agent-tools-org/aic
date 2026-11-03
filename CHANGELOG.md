# Changelog

All notable changes to `aic` are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## v0.3.1 (2026-11-03)

- Build task diffs from Git and aid JSON metadata, including in-place and untracked changes.

- Return exit code 4 for runtime errors while preserving audit verdict exit codes.
- Load user configuration with recursive project overrides and explicit command defaults.

## v0.3.0 (2026-11-03)

- Use the configured aid agent for L2, defaulting to codex.
- Dispatch read-only judges with whole context files and a fixed prompt that avoids aid report mode.
- Set explicit judge task profiles and disable nested audit and verification.
- Parse only a single JSON object in the final answer block; reject verdicts quoted in reports.
- Give L2 the whole diff: the 8 KiB cut let a blocking rule pass on a violation past the cut.
- A diff over 128 KiB is Inconclusive without calling a judge, and a judge that could not read every line answers inconclusive.
- Apply default Cargo checks and tests only to repositories containing Cargo.toml.
- Add four-target release binaries with SHA-256 checksums and document binary and Git installs.

## v0.2.3 (2026-04-12)

- Fixed: empty-diff early exit — `aic audit <task-id>` on a task with no diff content (e.g. pure text/research task invoked via `aid run --audit`) now returns Pass immediately without dispatching L2/L3 LLM calls. Previously the pipeline would run expensive LLM layers on an empty diff and typically land on Inconclusive or hallucinated Fail.
- Fixed: `aic init` and the committed sample `.aic/config.toml` now reflect v0.2.x defaults — single `codex` L3 model, `consensus_threshold = 1`, `budget_usd_per_audit = 5.0`. The stale v0.1 defaults in the committed sample caused manual audits from inside the aic repo to dispatch a non-existent `claude-haiku` agent and hit unnecessary swap disagreement.

## v0.2.2 (2026-04-12)

- Fixed: L1 now returns Pass with a single synthesized `skipped` Evidence when no commands are configured, instead of Inconclusive. This prevents an empty layer from poisoning the aggregator's final verdict in dogfood scenarios where the audit runs on diffs from an external repo without a local test suite.

## v0.2.1 (2026-04-12)

- Stopped `repair_partial_json` from turning tool-call payload JSON into synthesized fail verdicts; unstructured payloads now fall back to inconclusive instead.
- Strengthened the L2 and L3 review prompts to forbid file reads, directory listing, greps, tool calls, and missing-context failures outside the provided diff.

## v0.2.0 (2026-04-12)

### Changed

- Raised the default `[integration] budget_usd_per_audit` from `2.0` to `5.0` after v0.1 dogfood showed real L3 audits cost roughly `$1.00-$2.50`.
- Trimmed the default L3 model list to `["codex"]` and lowered `consensus_threshold` to `1`, while still allowing users to opt back into multi-model consensus in config.
- Added a single forced retry for L3 model responses that contain no JSON object, with evidence details now recording `retry=0` or `retry=1`.

## v0.1.0 (2026-04-12)

Initial release. `aic` is a cross-audit CLI that turns the manual Cross-Audit
Protocol used alongside `aid` agent dispatch into a scriptable, layered
verification pipeline.

### Added

- **5-layer verification pipeline** that short-circuits on failure:
  - L0 Static — deterministic compile/lint checks (`cargo check`, `clippy`,
    secret scan, address verification)
  - L1 Test — project-scoped test commands with timeout + process-group
    termination (no zombie leaks)
  - L2 Checklist — LLM-evaluated `.aic/rules/*.md` rules with structured
    JSON verdicts
  - L3 Cross-LLM — swap-augmented multi-model adversarial review with
    consensus aggregation and verbosity normalization
  - L4 Evidence (scaffolding only — planned for v0.2)
- **`aic audit <task-id>` / `aic audit --diff <path>`** — end-to-end audit
  with persisted JSON result and markdown report at `.aic/audits/<task-id>/`.
- **`aic init`** — idempotent `.aic/` scaffolding with sample rule.
- **`aic report <task-id>`** — re-render a stored audit result as markdown.
- **`aic checklist list`** — list loaded rules.
- **`.aic/rules/*.md`** adversarial checklist format with YAML frontmatter
  (`id`, `severity`, `scope`, `layer`) and free-form markdown body as the
  LLM prompt.
- **`.aic/config.toml`** schema covering all five layers plus
  `[integration]` section for `aid` / `aib` hooks.
- **AuditRequest**: honors `skip_layers`, `budget_usd`, `artifacts` for
  flexible caller control.
- **`aid` subprocess dispatcher** that correctly reads model output via
  `aid show <id> --output` and parses real `$cost` + duration from aid
  metadata lines — not fragile stdout capture.
- **Robust JSON handling** — `extract_json_object` strips markdown fences
  and tolerates prose preamble/postamble; `repair_partial_json` synthesizes
  a minimal verdict from structured hints (e.g. `violated_rules=[...]`)
  when a small model fails to emit strict JSON.
- **Prompt injection defense** — all untrusted content (diff, artifacts,
  spec) is wrapped in `<DIFF_START>/<DIFF_END>` delimiters with a system
  prompt stating the delimited content is untrusted data, not instructions;
  literal delimiter tags in content are sanitized to hyphen variants.
- **Budget gating** — mid-pipeline cost check aborts before running
  expensive L3 if earlier layers exhausted the budget.
- **`aib` integration** — `aic audit --board-item <id>` transitions the
  matching ai-board work item to `blocked` / `review` based on verdict and
  attaches the audit report.
- **Swap-augmented L3 voting** — each model is invoked twice with the
  diff/spec positions swapped to null position bias; disagreeing votes are
  marked Inconclusive before aggregation.
- **Process-group kill on L1 timeout** — uses `process_group(0)` on unix
  and `killpg(pid, SIGKILL)` to tear down the full descendant tree,
  preventing zombie `cargo test` processes.
- **L3 rules plumbing** — rules with `layer: l3` in their frontmatter are
  filtered by `touched_files` and injected into the L3 adversarial prompt.

### Research grounding

The pipeline design reflects findings from 2023-2026 literature on LLM
self-correction, cross-LLM verification, and judge reliability:

- **SycEval** (~58% LLM judge sycophancy): motivated the external
  separate-process design — `aic` never asks an agent to audit its own
  output.
- **AlpacaEval 2.0 / JudgeLM / swap-augmentation**: verbosity and position
  biases are measurable; swap the artifact positions to null position bias,
  and truncate evidence lists to cap verbosity influence.
- **LDB / AI Breakpoints**: execution-grounded verification dominates
  reference-free prompting; L0/L1 deterministic layers gate the LLM layers.
- **Agentless / AutoCodeRover**: hierarchical pipelines beat free-form
  agents for repair tasks — `aic` is intentionally a rigid pipeline, not
  an agent.

### Known limitations (v0.1)

- L3 small models (e.g. `gemini-2.5-flash-lite`) are unreliable at
  producing strict JSON even with forceful prompts; they tend to divert
  into tool-call investigation workflows. Use `codex` or a stronger judge
  model for L3 in production.
- Default `budget_usd_per_audit = 1.0` is below the realistic cost of a
  2-model swap-augmented L3 run (~$1.00-$2.50). Raise to `5.0` for
  meaningful L3 audits.
- L4 Executable Evidence is scaffolded but not wired; `crpc` / external
  script integration is planned for v0.2.
- `aic audit --watch <task-id>` trajectory mode (real-time scoring during
  `aid` execution) is planned for v0.2 and requires an upstream `aid`
  event-stream API.
- The `aid --audit` flag for automatic post-run dispatch is tracked
  upstream at [agent-tools-org/ai-dispatch#98](https://github.com/agent-tools-org/ai-dispatch/issues/98).

### Tests

- 77 unit tests + 11 integration tests across `tests/audit_cli.rs`,
  `tests/cli_help.rs`, `tests/pipeline_request.rs`, `tests/short_circuit.rs`.
- `cargo clippy -- -D warnings` clean.

### Development process

This v0.1 was built via a dev-manager-driven workflow that dispatches work
to `aid` agents, then uses `aic` (dogfood) to audit its own output. The
development produced three rounds of adversarial cross-audit that caught
real defects — including a regression (evidence list silently discarded)
introduced by an earlier fix round. Full architecture write-up in
`docs/architecture.md`; dogfood results in `docs/dogfood.md`.
