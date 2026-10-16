# Plan to match the leading suit-combination calculators

Research date: 2026-10-16. Baseline: commit `d3c85b4`.
Sources and caveats are in the [research note](suit-calculator-research.md).

**References:** SuitPlay for analysis results, SuitC and Bridge CCAnalyser
for problem constraints, BridgeTricks for interactive exploration. No verified
ranking or shared benchmark exists, so "best" is only claimable for a measured
scope. Keep this engine; build correctness and performance evidence before
growing the game model.

## Model

Our model is a pure declarer strategy against an omniscient defense that may
falsecard, as in the [FINESSE paper](https://cdn.aaai.org/AAAI/2000/AAAI00-030.pdf).
It says nothing about mixed-strategy equilibria.

Results are comparable only when goal, prior weights, entries, first leader,
fixed cards, ruffs and defense model match. `P(tricks >= N)` is not
`P(tricks = N)`; one strategy's value is not a sum of independently optimized
targets; double-dummy averages are upper bounds, not policies.

## Milestones

1. **Core parity:** independently checked single-dummy results and playable
   lines for trick targets and exact expected tricks, with usable response
   times and inspectable explanations.
2. **Product parity:** the leaders' constraints, line comparison, MP/IMP
   scoring and problem coverage.

## Current position

| Capability | Today ([lib.rs](../src/lib.rs), [tests](../tests/solver.rs), [wasm](../web/src/lib.rs), [app.js](../web/app.js), [CLI](../examples/solve.rs)) | Gap |
| --- | --- | --- |
| Hidden-card analysis | Pareto vectors keep one strategy across indistinguishable layouts; targets (0/1 vectors) and `ExactExpected` are exhaustive | Little independent validation |
| Expected tricks | `Expected` = combination of target lines plus an upper bound; exact mode separate | Fast EV is not always optimal (`A8 / JT62`: 1.644 vs 1.651) |
| Probabilities | Fixed 13/13 vacant places, `TOTAL = C(26,13)` hard-coded in engine and browser | No vacant places, known cards, length or HCP constraints |
| Entries and play | Either hand may lead each round (`declarer_moves`); `advance` resets the leader; no ruffs | No finite entries, forced leader, limited rounds, ruffs |
| Alternative lines | Values for every equivalent opening lead; one plan per goal | No line browser, line-by-layout matrix or comparison |
| Goals | At-least-N, bounded EV, exact EV | No MP/IMP line comparison |
| Coverage | At most eight missing cards (`TooManyMissing`) | `AQ / 2` is rejected |
| Interface | Browser trainer, custom holdings, card grading, revealable tree, CLI, serializable Rust API | No analysis-first view, free defender branching, sharing or export |
| Runtime control | Worker termination; random-deal and exact-EV budgets | No engine budget, retained progress or memory contract |
| Validation | 11 integration tests, a few published combinations | No reference corpus, differential runner or benchmark; CI only deploys Pages |

`Game::defend` is a heuristic tie-break for the trainer, not a defense proof.
Guarantees must be checked by following the returned policy against every
legal defensive reply.

## Baseline measurements

`cargo test --all-features --release`, then `target/release/examples/solve
NORTH SOUTH [TARGET|ev|exact]` on an AMD Ryzen 7 8700F, Rust 1.99.0. Internal
analysis time of one native release run; spot checks, not percentiles.

| Holding / goal | Result | Time |
| --- | --- | --- |
| `KQT32 / 654`, all goals | >=2 98.04%, >=3 89.57%, >=4 55.39%, EV 3.430 | 21 ms; matches the [SuitC guide](https://bridge.esmarkkappel.dk/suitc/SuitC%20User%20Guide_9.pdf) at displayed precision |
| `A8 / JT62`, fast EV | 1.644, bound 1.651 | 36 ms |
| `A8 / JT62`, exact EV | 1.651 | 9 ms |
| `A953 / KJ8`, >=3 | 83.85% | 1.55 s |
| `KJ84 / T63`, >=2 | 81.49% | 2.53 s |
| `KJ84 / T63`, >=3 | 19.56% | 148 ms |
| `KQ85 / T73`, >=3 | 24.55% | 89 ms |
| `KQ85 / T73`, >=2 | none | timeout at 30 s |
| `Q876 / AT32`, exact EV | 2.616 | 15 ms; fast mode found it but its 2.644 bound did not prove it |
| `AQ / 2`, >=2 | `TooManyMissing` | panic in the example CLI |

The README's 600-case EV study is not reproducible from tracked files (no
seed, harness or raw results). Competitors were not run or timed here.

## Phases

### 1. Evidence

- `tests/fixtures/` with one record per case: holdings, goal, constraints,
  source, version, date, defense model, expected value or published rounding
  interval, acceptable opening plays. Seed it with the SuitC example, the
  Pavlicek/Todd cases, restricted-choice and falsecard cases, and
  `A8 / JT62`. Grow to 100+ independently sourced cases, then any reusable
  published sets. Generated self-comparisons are not independent evidence.
- A plain uncompressed small-position oracle, and a policy evaluator that
  follows a returned plan while minimizing over every legal defense per
  layout. A target plan ending at `Made`/`Hopeless` is not an EV strategy.
- A seeded benchmark corpus stratified by missing cards, honor pattern,
  lengths and goal, including every slow case. Record p50/p95/max time, peak
  memory, timeouts, state counts, max antichain size, proof rate, EV gap.
- Inspect every disagreement; never paste a competitor's number into a fixture.

**Done:** CI runs tests and the corpus; every fixture traceable; zero
unexplained disagreements in shared scope; benchmark results reproducible.

### 2. Practical exact analysis, unavoidable proof status

- Profile `maximal`, `feasible_t`, `feasible`, vector cloning and memo growth
  on the frozen corpus; optimize from measurements (packed bitsets, fewer
  allocations, dominance indexing). Keep the old exact path for diffing.
- If the tail remains, study FINESSE's tactical move generation and beta
  reduction. Adopt nothing without an equivalence check; full search stays
  the fallback.
- Engine budgets and cooperative cancel returning exact / bounded /
  incomplete, with a valid incumbent and admissible bound where available.
  Budget exhaustion never yields an "exact" answer. Unlimited API stays the
  default.
- Carry status and bounds through `Analysis`/`Lead`, CLI, wasm, worker, UI.
  A global EV certificate does not certify every alternative lead. Keep
  finished targets after cancel; solve a requested target without waiting for
  the rest.

**Done:** nothing unproved is labelled optimal; cancel within 250 ms in the
browser; no silent memory pruning. Targets (engineering, not competitor
figures): 7–9 missing cards p95 <=1 s native, <=2 s browser; visible
answer or status within 2 s always. Difficult-corpus gates set after phase 1.

### 3. Trainer becomes an analysis tool

Reuse `Plan`, `Lead` and the renderer. Add an Analyse view: all goals at
once, goal switching, a play explorer where the user picks any legal defender
reply. Show distinct strategies with per-layout guarantees and a flag when
enumeration is incomplete, a line comparison table, a trick distribution per
complete strategy, and shareable URL/JSON (cards, constraints, goal). Generate
bridge-language summaries from verified trees and keep the tree for audit.

**Done:** reproduce a reference position, inspect the justifying branches,
compare two lines, share it, without playing a random layout first. The
distribution evaluates the same policy as the tree.

### 4. Constraints and coverage

In order, each with an independent fixture before the next:

1. Vacant places and known East/West cards; then length and HCP
   restrictions (state whether HCP means this suit or the whole hand).
   Condition and renormalize; reject an empty layout set.
2. Forced first leader and finite entries: model the previous trick's winner
   and entry consumption explicitly.
3. Fixed first plays, finite rounds, specified discards/ruffs with documented
   outside-suit assumptions; no inherited ambiguous ruff semantics.
4. Lift the eight-missing-card limit for ordinary holdings; size
   representations by the compressed layout space. Cover short holdings,
   empty hands and all-card holdings.

`Structure::weight`, `TOTAL`, `State`, class merging, memo keys, game
sampling, CLI, wasm and UI must agree. Nonuniform priors or rank-specific
constraints may break the equivalence-class argument: keep the distinctions
inference needs and verify against uncompressed layouts. Constrained analysis
gets its own denominator; the default 13/13 contract is unchanged.

**Done:** each option matches independent examples; defaults reproduce
phase 1; `AQ / 2` solves; unsupported option combinations are rejected clearly.

### 5. SuitPlay scoring and the report

Line-versus-line MP and IMP comparison over per-layout outcomes. MP is not
renamed EV; pairwise preference need not give one dominating line, so return
the undominated set and represent "no single best line". Take SuitPlay's
contract and suit-trick inputs and its documented IMP convention. First
fixtures: `AQ1098 / 5432` (EV vs MP) and `AJ98 / K32` (no best MP line).

**Done:** published MP/IMP and cyclic cases agree under matching assumptions;
target/EV fixtures still pass.

Then publish a versioned comparison report: coverage, disagreements,
hardware, native and browser timings, memory, timeouts, proof rates. Claim a
tie only for the measured scope.

## Next step and exclusions

Phase 1, then the highest-impact measured runtime fix. This plan changes
documentation only.

Not needed: rewrite, server backend, new framework, GPU search, full-deal AI,
bidding inference. Mixed-strategy equilibria are a separate research question.
