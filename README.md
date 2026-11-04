# aic

**Cross-audit outputs from `aid` agent dispatch with a 5-layer verification pipeline.**

## Why aic?

LLM agents are efficient but fallible. Research shows that as tasks get harder, self-reflection fails: **SycEval** found 58% sycophancy in LLM judging, and **AlpacaEval 2.0** highlights significant length bias and position bias in model-based evaluation. Manual "Cross-Audit Protocols"—where one agent's work is rigorously critiqued by others—are effective but become a scalable pain for human orchestrators.

`aic` turns this protocol into a scriptable, CI-grade pipeline. It provides deterministic gates and swap-augmented LLM judging to ensure that code changes are not just plausible, but correct. It complements `aid` (dispatch) and `aib` (backlog) as the fourth pillar of the AI dev toolchain.

## Install

```bash
curl -fsSL https://aid.agent-tools.org/install.sh | bash
```

The script installs aic alongside aid. Or install from source:

```bash
cargo install --git https://github.com/agent-tools-org/aic --tag v0.3.2 --locked
```

The crates.io package named `aic` is a different project.

## 60-Second Quickstart

1. **Initialize**: `aic init` to scaffold the `.aic/` directory.
2. **Author Rules**: Edit `.aic/rules/*.md` to define project-specific adversarial checks.
3. **Audit**: Run an audit on a standalone diff or a completed `aid` task.
   ```bash
   aic audit --diff fix-bug.patch
   ```

Task audits (`aic audit <task-id>`) require an aid release supporting
`aid show <task-id> --patch`. L0 and L1 run in the current directory, so run a
manual audit from the task’s checkout. Failure to collect a patch is a runtime
error (exit 4); an empty patch passes without calling a judge.

### Exit Codes
- `0`: **Pass** — All layers passed or only advisory failures.
- `1`: **FailBlocking** — Hard failure in a critical layer or rule.
- `2`: **FailAdvisory** — Non-blocking warning detected.
- `3`: **Inconclusive** — Pipeline could not reach a verdict (e.g., budget exceeded).
- `4`: **Runtime error** — An error prevented completion; details are printed to stderr.

Clap usage errors retain their own exit codes.

## The 5 Layers

Verification is layered by cost and latency. Each layer can short-circuit the pipeline on failure.

| Layer | Type | Cost | Latency | Purpose |
|-------|------|------|---------|---------|
| **L0** | Static | Free | <5s | Deterministic checks: `cargo check`, `clippy`, secret scanning. |
| **L1** | Test | Free | 10-120s | Execution: Runs project test suites and verification scripts. |
| **L2** | Checklist | ~$0.01 | ~5s | Rule-based auditing: Evaluates `.aic/rules/*.md` via fast LLMs. |
| **L3** | Cross-LLM | ~$0.50 | ~30s | Multi-agent debate: Swap-augmented voting across N models. |
| **L4** | Evidence | Varies | ~10s | (Future) Executable claims: `crpc` calls and trace verification. |

## Config Reference (`.aic/config.toml`)

User defaults are read from `$XDG_CONFIG_HOME/aic/config.toml`, falling back to
`$HOME/.config/aic/config.toml` when XDG_CONFIG_HOME is unset, empty or relative. The project
file (`--config`, default `.aic/config.toml`) overrides user settings key by key:
TOML tables merge recursively, and arrays and other values are replaced whole.
Either file may be absent; malformed files are errors naming the file.
`aic init` ignores user settings and writes defaults for the current repository.

Task audits load `.aic/config.toml`, rules (including relative rule paths outside `.aic/`),
and Cargo default detection from the first attempt's recorded start commit returned by
`aid show <task-id> --start-commit`.
Missing base config uses user config and defaults; checkout additions apply to later tasks.
Base lookup errors fail with exit 4 and require an aid version supporting `--start-commit`.
Relative rule paths escaping the repository are rejected. Absolute user-config rule paths
remain external; project-config rule paths must be relative to the repository.
A task's edits to repository content, including `.aic/`, `Cargo.toml`, and Git replace refs,
cannot change its own audit configuration. This does not contain an unsandboxed agent running
as the same user that can rewrite aid's database or the user config. Config-defined L0 and L1
commands still run the delivered code.
Reports are written to `.aic/audits/` in the working tree.
`layers.l4.scripts` is currently unused. L0/L1 command strings execute as shell commands.
An explicit top-level `--config <path>` uses the caller's file and checkout paths instead;
`--diff <path>` keeps loading the default config from the working tree. `aic init` is unchanged.

L2 `model` and L3 `models` are aid agent names. L0/L1 `commands` omitted from the merged config use
Cargo defaults only when the repository root contains `Cargo.toml`; otherwise
they skip commands. Explicit lists, including `[]`, always win. `aic init`
writes commands for the current repository. A user-level `commands = []` disables
that layer’s commands even in Rust repositories unless the project overrides it.

```toml
[layers.l0]
commands = ["cargo check", "cargo clippy -- -D warnings"]
secret_scan = true

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
consensus_threshold = 1

[integration]
aib_auto_block = true
budget_usd_per_audit = 5.0
```

## Rule Authoring

Rules live in `.aic/rules/*.md` with YAML frontmatter.

```markdown
---
id: no-unwrap-in-prod
severity: advisory
scope: ['src/**/*.rs']
layer: l2
---
# No unwrap() in production code
Flag uses of .unwrap() outside test modules. Prefer .expect() or `?`.
Ask the model: "Does the diff add any .unwrap() calls outside of tests?"
Pass if no new unwraps in non-test code. Fail otherwise.
```

## Integration

- **ai-board**: Use `aic audit --board-item <id>` to automatically transition `aib` items to `blocked` (on failure) or `ready-for-review` (on pass).
- **v0.2 Preview**: Future `aid run --audit` flag will enable seamless post-generation auditing.

## Architecture

`aic` follows an external, non-self-reflective auditing pattern grounded in research like **Agentless** and **LDB**. See `docs/architecture.md` for the full design doc and `docs/dogfood.md` for the v0.1 dogfood validation run against real smart-router bug fixes.

```
aid task output (diff + artifacts)
       │
       ▼
 ┌───────────────┐     ┌───────────────┐     ┌───────────────┐
 │   L0 Static   │ ──▶ │    L1 Test    │ ──▶ │ L2 Checklist  │
 └───────────────┘     └───────────────┘     └───────────────┘
       │                     │                     │
       ▼                     ▼                     ▼
 ┌───────────────┐     ┌───────────────┐     ┌───────────────┐
 │  Verdict/Log  │ ◀── │  L4 Evidence  │ ◀── │  L3 Cross-LLM │
 └───────────────┘     └───────────────┘     └───────────────┘
```

## Research Grounding

`aic` is built on principles from:
- **SycEval**: Mitigating judge sycophancy (58% base rate) via reference-backed critiques.
- **AlpacaEval 2.0**: Nullifying position/length bias via swap-augmentation.
- **LDB / Agentless**: Prioritizing deterministic execution traces and static gates over raw LLM intuition.

## Limitations (v0.1)

- Small LLM judges (e.g. `gemini-2.5-flash-lite`) struggle to produce strict JSON output; use a capable model such as `codex` for L3 in production.
- Default `budget_usd_per_audit = 1.0` is tight for a real L3 run; raise to `5.0` when enabling L3 in config.
- L4 executable evidence (native `crpc` / on-chain verification) is scaffolded but not yet wired.
- No real-time trajectory watch mode (planned for v0.2).
- `aid run --audit` upstream flag is tracked at [agent-tools-org/ai-dispatch#98](https://github.com/agent-tools-org/ai-dispatch/issues/98); not yet shipped.

## Roadmap (v0.2)

- **Trajectory Critic**: Real-time scoring during `aid` execution.
- **L4 First-Class**: Native `crpc` and `verify-addresses` integration.
- **Checklist Gen**: Auto-generate `.aic/rules/` from historical diffs.
- **Single-Claim Verify**: `aic verify <claim>` for targeted fact-checking.
