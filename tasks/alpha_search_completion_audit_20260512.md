# Alpha Search Completion Audit - 2026-05-12

## Objective

Build a system that can automatically generate and explore alpha factors, then
produce a strategy that is eligible for profitable dry-run promotion.

This audit intentionally separates discovery evidence from promotion evidence.
Alpha-search reward, MCTS rank, or a passing candidate row is not enough to
claim a profitable strategy.

## Current Verdict

Status: `not complete`.

The repo can automatically generate and explore alpha candidates through the
hosted Factor Walk-Forward V2 path. The latest evidence still produces
`autofactor-strategy-handoff.json status=blocked` and
`recommended_action=do_not_promote`, so there is no promotable or proven
profitable strategy yet.

The hard blocker is recorded replay/dry-run parity for:

`pm5d.threelayer.settlement-probability-btc-eth.dryrun`

Latest parity runs:

- run: `25683379408`
- workflow: `recorded-replay-parity.yml`
- status: `completed`
- result: workflow success, strict parity failed
- resolved auto window: `2026-05-11T13:46:06Z` ->
  `2026-05-11T16:56:00Z`
- reason: the auto window selected dry-run rows outside the available recording
  coverage, so replay produced no shared event/order/fill rows
- deploy impact: none; this run was a replay comparison, not a deploy

- run: `25685537480`
- workflow: `recorded-replay-parity.yml`
- status: `completed`
- result: workflow success, strict parity failed
- manual window: `2026-05-11T19:47:00+08:00` ->
  `2026-05-11T21:12:00+08:00`
- shared orders/fills: `18 / 18`
- blocking flags:
  `events_present_in_dryrun_missing_from_replay`,
  `orders_present_in_dryrun_missing_from_replay`,
  `runtime_evidence_field_mismatches`,
  `settlement_exit_price_mismatches`
- interpretation: the window mismatch is no longer the only issue. The current
  dry-run evidence still contains entry/settlement behavior that current replay
  cannot reproduce, so promotion remains correctly blocked.

## Prompt-To-Artifact Checklist

| Requirement | Evidence | Status | Notes |
| --- | --- | --- | --- |
| System can generate alpha candidates | `factor-walk-forward-v2-hosted-artifact.yml` emits alpha-search artifacts under `factor-walk-forward-v2/alpha-search/` | `ready` | Verified by runs `25683730944`, `25683858420`, `25683945041`, and `25684811262`. |
| System can explore with MCTS-style continuation | `mcts-expansion-plan.json`, `alpha_search_plan_run_id`, `chain_next_run`, and `alpha-search-chain/chain-decision.json` | `ready` | Run `25683730944` dispatched `25683858420`; run `25683858420` dispatched `25683945041`. |
| System records interpretable search artifacts | `search-space.json`, `llm-priors.json`, `candidate-expressions.json`, `rejected-expressions.json`, `tree-trace.json`, `node-metrics.json`, `search-feedback.json` | `ready` | These are uploaded in hosted artifacts. `llm-priors.json` is a typed placeholder prior, not free-form live LLM generation. |
| System summarizes search-chain evidence | `scripts/summarize_alpha_search_chain.py`, `alpha-search-chain/summary.json`, `alpha-search-chain/summary.md` | `ready` | PR #428 added the script. PR #429 wired it into the hosted workflow. Run `25684811262` proves the summary files are uploaded. |
| Search found a stronger candidate branch | run `25683858420` | `partial` | Best reward improved to `4.894975850946932` with `mcts_mcts_auto_settlement_conservative_settlement_edge_x_near_strike_near_strike_near_strike`. |
| Promotion gate blocks unsafe candidates | `autofactor-strategy-handoff.json` | `ready` | Recent runs report `status=blocked`, `recommended_action=do_not_promote`, and `ready_handoff_count=0`. This is correct behavior, not a search failure. |
| Replay/dry-run parity is ready | `recorded-replay-parity.yml` artifact | `blocked` | Runs `25683379408` and `25685537480` completed, but no ready parity artifact exists for the current target lane. The latest manual-window run has shared rows, yet still reports runtime field mismatches and settlement-exit mismatches. |
| A dry-run config PR can be generated from ready handoff | `create_config_pr=true` path in hosted workflow | `blocked` | The path exists, but it is fail-closed until handoff status is `ready`. |
| A profitable strategy has been produced | ready handoff plus parity plus dry-run/executable evidence | `not ready` | No artifact currently proves executable profitability with replay/dry-run parity. |

## Latest Alpha-Search Evidence

All runs used:

- git ref: `main`
- snapshot: `25642459432`
- symbols: `BTCUSDT,ETHUSDT`
- window: `2026-04-24..2026-04-30`
- stake: `15`

| Run | Candidates | Passed | Best reward | Best selected factor | Handoff | Action |
| --- | ---: | ---: | ---: | --- | --- | --- |
| `25683730944` | 183 | 32 | 4.81954070569323 | `auto_settlement_conservative_settlement_edge` | blocked | do_not_promote |
| `25683858420` | 231 | 68 | 4.894975850946932 | `mcts_mcts_auto_settlement_conservative_settlement_edge_x_near_strike_near_strike_near_strike` | blocked | do_not_promote |
| `25683945041` | 203 | 47 | 4.81954070569323 | `auto_settlement_conservative_settlement_edge` | blocked | do_not_promote |
| `25684811262` | 183 | 32 | 4.81954070569323 | `auto_settlement_conservative_settlement_edge` | blocked | do_not_promote |

## Landed System Improvements

- PR #426: `recorded-replay-parity.yml` supports `since=auto` and
  `until=auto`, writes `resolved-window.json`, and records the resolved window
  in artifacts/comments.
- PR #427: `docs/ALPHA_FACTOR_SEARCH_CICD.md` now states that alpha generation,
  MCTS planning, and hosted chaining exist; promotion evidence is the blocker.
- PR #428: `scripts/summarize_alpha_search_chain.py` summarizes downloaded
  hosted artifacts into chain-level JSON and Markdown.
- PR #429: hosted Factor Walk-Forward V2 now uploads
  `alpha-search-chain/summary.json` and `summary.md` for each run.

## Remaining Actions

1. With explicit operator approval, deploy current `main` to the protected
   `tango-1-1` dry-run path while keeping live paused.
2. Collect a fresh settlement-probability BTC/ETH dry-run sample from the
   newly deployed binary/config.
3. Rerun `recorded-replay-parity.yml` against that fresh sample. Use `auto`
   only after confirming the recording and dry-run coverage overlap; otherwise
   use the explicit fresh recording window.
4. If parity is ready, rerun hosted alpha-search/promotion with
   `replay_parity_run_id=<ready-run>`.
5. If handoff becomes `ready`, create the dry-run config PR through the
   existing `create_config_pr=true` path.
6. Only after dry-run/replay parity and executable evidence remain clean should
   any live deployment discussion start.

## Completion Rule

Do not mark the user objective complete until all of the following are true:

- alpha-search artifact bundle exists for the selected candidate;
- `autofactor-strategy-handoff.json` reports `status=ready`;
- recorded replay/dry-run parity reports strict readiness for both runtime and
  event evidence;
- the dry-run config PR is generated from the ready handoff and passes CI;
- executable dry-run evidence supports profitability after costs, fills,
  settlement, and drawdown constraints.
