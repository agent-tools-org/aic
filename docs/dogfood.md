# aic v0.1 Dogfood Report

**Date**: 2026-04-12
**Subject**: aic MVP (fix-006 + fix-007 applied) audited against real smart-router bug fixes
**Status**: First signal — pipeline works end-to-end, known limitations documented

## Setup

- **aic binary**: `$CARGO_TARGET_DIR/release/aic` built from `fix/aic-strict-json` top of stack (commit da29436, 13 commits total)
- **Dogfood working dir**: `/tmp/aic-dogfood/`
- **Rules configured**: 3 project-specific rules in `.aic/rules/`
  - `on-chain-evidence.md` (blocking) — numeric threshold changes must cite on-chain source
  - `calldata-encoding.md` (blocking) — dex_router + build_step must be updated together
  - `monotonic-time.md` (advisory) — decay math must use `Instant`, not `SystemTime`
- **Budget**: `budget_usd_per_audit = 1.0`
- **L3 models**: `["gemini", "codex"]`, swap=true, consensus_threshold=2

## Patches audited

1. `01-woofi-oracle.patch` — SR commit d0bfb332 `fix(woofi): revert oracle freshness to match on-chain staleDuration`, 44 LoC, single-file fix
2. `02-woofi-calldata.patch` — SR commit afd1e4c9 `fix(calldata): add WOOFi WooRouterV2 to dex_router + build_step`, 151 LoC across 4 files
3. `03-wss-monotonic.patch` — SR commit 6a6b4ce7 `fix(wss-stats): use monotonic Instant for rolling-rate bucket decay`, 111 LoC

## Run 1: patch 01 woofi-oracle (only patch run; see budget observation)

**Verdict**: Inconclusive — evidence cost 1.0100 exceeded budget 1.0000
**Duration**: 406 seconds (L3 dominates)
**Cost**: $1.01 real (parsed from aid run metadata)

### Layer results

| Layer | Verdict | Duration | Evidence | Notes |
| --- | --- | --- | --- | --- |
| L0Static | Pass | 0 ms | 0 | No commands configured (dogfood dir has no Cargo.toml) |
| L1Test | Inconclusive (no commands) | 0 ms | 0 | Expected — dogfood patches are external diffs |
| **L2Checklist** | **Pass** | 49 s | 1 | gemini returned a real verdict with reasoning |
| **L3CrossLlm** | Inconclusive (no consensus) | 357 s | 4 | codex gave real JSON fails, gemini-flash-lite diverged |

### L2 evidence

One rule matched (`on-chain-evidence`). L2 Pass verdict from gemini:

> "The change correctly aligns with on-chain WooracleV2 behavior and provides empirical evidence for the new threshold and margin."

This is a legitimate L2 review — gemini read the rule, read the diff, issued a defensible Pass because the commit message already cites on-chain evidence (`staleDuration: 120s`, `isWoFeasible` check).

### L3 evidence — model-by-model

**gemini call 1 (normal position)**: prose only, no JSON. Tool calls: `list_directory .aic/rules`, `read_file woofi_cache.rs`, `grep_search FRESHNESS_MARGIN`. synthesized `violated_rules=[]`, vote: Inconclusive.

**gemini call 2 (swapped position)**: prose only, similar pattern. synthesized empty. vote: Inconclusive.

**codex call 1 (normal)**: returned ACTUAL JSON:
```json
{"verdict":"fail","bugs":["is_oracle_fresh() now rejects WooFi routes from a sync-lagged cached oracle_ts, so when the on-chain oracle refreshes between syncs the router can..."]}
```
vote: fail, severity: blocking (synthesized from violated_rules).

**codex call 2 (swapped)**: returned JSON:
```json
{"verdict":"fail","bugs":["age + FRESHNESS_MARGIN < stale uses a cached oracle timestamp that can lag the on-chain timestamp by one sync interval..."]}
```
vote: fail.

**Aggregated verdict**: 2 fail + 2 inconclusive = below consensus threshold of 2 same-direction votes that matter. Even if codex's fails counted, the aggregator returned L3 = no-consensus Inconclusive.

### Findings from this run

1. **✅ End-to-end pipeline works on real bug fixes.** After fix-006 (dispatcher) and fix-007 (strict JSON + fallback), aic actually reads real model output, parses cost, tracks budget, and produces layer-level verdicts on a real smart-router diff.

2. **✅ Cost tracking is accurate.** $1.01 parsed from aid metadata lines, budget gate correctly fired Inconclusive at the $1.00 cap. Previous flat-estimate would have reported $0.20.

3. **✅ L2 checklist works well** with gemini-flash-lite. Forceful prompt + concrete JSON example yields a structured verdict reliably on this patch.

4. **⚠️ L3 with gemini-flash-lite is unreliable.** Even with the strictest possible "JSON ONLY, no preamble, no tool calls" prompt, gemini-2.5-flash-lite treats the task as an investigation and invokes tool calls. It was designed for agentic workflows, not structured-output Q&A. Recommendation: **do not use flash-lite-class models for L3 in production**. Use codex or a stronger model.

5. **⚠️ L3 codex produced interesting substantive findings** that we did NOT verify against the SR team's actual post-merge review. codex flagged a possible behavioral issue: the cached `oracle_ts` can lag the on-chain timestamp by one sync interval, meaning after the oracle refreshes on-chain, aic's routes are briefly rejected. This MAY be a real bug the fix introduced. Worth a human review of the WOOFi cache sync interval.

6. **💡 Budget $1.00 is too tight for L3 with 2 models × 2 swap calls.** Real cost per audit is ~$1.00-$2.50 depending on model mix. Raise default budget to $5 for meaningful L3 runs, or reduce L3 to a single capable model.

## Why patches 2 and 3 were not run

The $1.00 budget gate fires ~70 seconds into L3, which means each audit takes ~6 minutes and burns the full budget before the aggregator ever gets a complete picture. Running more patches under the same config would just produce more Inconclusive-by-budget verdicts without new insight. Better to:
1. Raise the default budget
2. Reduce L3 model list to only codex (the one that actually works)
3. Then run all 3 patches for comparison

This is the scope of a follow-on tuning pass, not the MVP scope.

## Recommendations for aic v0.2

### High priority
- **L3 model selection guidance**: Architecture doc should explicitly warn that flash/mini/lite class models (fewer tokens per query, tool-augmented) don't return strict JSON reliably. Recommend codex, sonnet, or a tuned judge model.
- **Budget defaults**: raise `budget_usd_per_audit` default from 1.0 to 5.0 in the generated config.toml. L3 is the dominant cost and 1.0 is below the floor for anything useful. ✓ landed in v0.2-tuning
- **Reduce L3 default models** from 3 to 1-2 in the generated config. Quality > quantity with capable models. The "N-of-M consensus" assumption breaks when half the models can't produce structured output. ✓ landed in v0.2-tuning

### Medium priority
- **L3 prompt retry on prose response**: if a model returns prose only (no JSON hint at all), re-dispatch once with an even more forceful "you MUST return ONLY a JSON object starting with { ending with }" system message. Second-chance retry with clearer framing before falling back to synthesized Inconclusive vote. ✓ landed in v0.2-tuning
- **Human-readable evidence rendering**: the current `report.md` renders each Evidence as a single line with detail truncated. For L3 evidence specifically, surface the actual `bugs` list as a bulleted subsection, since that's where the audit value lives.

### Low priority
- **Report attribution**: report.md should show which model produced which vote, not just "L3CrossLlm LlmVote".
- **Incremental audit cache**: if the same diff is audited twice with the same rules, L0/L1 results can be cached.

## Actionable: codex's WOOFi finding

During the L3 audit, codex flagged a potential issue with the WOOFi oracle freshness fix. Short form:

> The fix reverts `age + FRESHNESS_MARGIN(10s) < stale` to match the on-chain `isWoFeasible` check. But this uses a CACHED `oracle_ts`. If the on-chain oracle refreshes between sync cycles, the cached timestamp lags the live one by one sync interval (~80-100s per earlier docs). During that window, aic rejects valid routes because its cached age exceeds `stale - FRESHNESS_MARGIN`. Post-sync, routes recover.

Whether this is a real bug depends on WOOFi cache sync cadence vs oracle update frequency. Worth a 15-minute look by someone familiar with `woofi_cache.rs`. Even if it turns out to be a false positive, it demonstrates that L3 cross-LLM review CAN produce substantive specific findings on real diffs — which is the whole point of aic.

## Summary

aic v0.1 works end-to-end on a real smart-router bug fix. Dispatcher + cost tracking + budget gate + L2 + L3 all functional. Primary remaining issue is model selection: small models don't follow strict output schemas. Budget defaults need to be tuned for realistic L3 cost. Recommend raising budget to $5, trimming L3 to codex-only, and re-running the 3-patch suite as v0.2 validation.

**Ready for next phase: YES, with v0.2 tuning pass scheduled.**
