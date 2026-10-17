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
| Entries and play | Unlimited entries by default; for trick targets, a forced first leader and finite entries per hand (`Constraints::first`, `Constraints::entries`); no ruffs | No limited rounds, fixed plays, ruffs; no entries for EV |
| Alternative lines | Values for every equivalent opening lead; one plan per goal | No line browser, line-by-layout matrix or comparison |
| Goals | At-least-N, bounded EV, exact EV | No MP/IMP line comparison |
| Coverage | At most eight missing cards (`TooManyMissing`) | `AQ / 2` is rejected |
| Interface | Browser trainer, custom holdings, card grading, revealable tree, CLI, serializable Rust API | No analysis-first view, free defender branching, sharing or export |
| Runtime control | Worker termination; random-deal and exact-EV budgets | No engine budget, retained progress or memory contract |
| Validation | 44 reference cases in [tests/fixtures/cases.json](../tests/fixtures/cases.json) (SuitC, Todd, Pavlicek, hand derivations); a policy evaluator ([tests/fixtures.rs](../tests/fixtures.rs)) replays every plan on every layout against every legal defence; a seeded 116-combination corpus run by [examples/bench.rs](../examples/bench.rs); CI runs tests and the corpus | Published numbers exist for few cases (Pavlicek states plays, not percentages); no uncompressed oracle beyond the evaluator; corpus timings from one machine |

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

### Corpus baseline (2026-10-16)

`examples/bench.rs run tests/fixtures/corpus.txt 30`, same machine, one
process per run, 861 runs over 116 combinations (every target, `ev`,
`exact`); raw rows in [bench-2026-10-16.tsv](bench-2026-10-16.tsv).

| Measure | Value |
| --- | --- |
| Finished / timeout / error | 854 / 7 / 0 |
| Time p50 / p95 / max (finished) | 4.1 ms / 114 ms / 13.3 s |
| Peak memory | 212 MB (`KJ85 / T63` ev) |
| Memoized states / widest antichain | 237,745 / 57,877 (`AQ95 / T73` exact) |
| `ev` proven by its bound | 94 / 114 (82.5%) |
| EV gap, exact minus line (113 pairs) | mean 0.003, max 0.091 (`AQT8 / 642`) |
| Timeouts (30 s) | `KQ85 / T73` t2, ev, exact; `KT97 / 53` t1, ev, exact; `A953 / KJ8` exact |

New from the stratified sample: `KJT85` opposite a void takes 4–7 s per
goal, so void hands are not trivially cheap, and the EV line's worst miss is
larger than the earlier 600-combination study suggested (0.057).

## Phases

Phases run in this order. Trick targets are the flagship: phase 3 builds
constraints and coverage for them alone, and phase 4 treats expected tricks
as a game of its own. The analysis view comes after both because it must
expose them; building it last builds it once, on the final engine API. Phases 1 and 2 keep the numbers earlier commits
cite; the rest were renumbered on 2026-10-18 to match the order (constraints
was 4, scoring 5, the analysis view 3, the report part of 5).

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

**Status 2026-10-16:** fixtures, evaluator, corpus and CI exist. The
evaluator agrees with every claimed score (all goals, 44 reference cases and
60 seeded random combinations). Three Pavlicek plays disagreed with the
engine and are explained in the fixture notes: `AJ983 / Q2` (his Q is the
most-tricks lead, not the four-trick lead), `Q54 / KT32` (his line is best
for three tricks; for two the engine gains one West singleton by leading
toward the KT) and `AJT3 / Q92` (his objection to the Q is an entry argument
outside the model). The uncompressed oracle is not built: the evaluator
checks guarantees, not optimality, so optimality evidence is still the
published values and derivations. Corpus results: see
[bench-2026-10-16.tsv](bench-2026-10-16.tsv) and the summary below.

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

**Status 2026-10-18:** done in `47b9218` (speedups, uncompressed oracle),
`bbfc43f` (budgets, cancel, proof status on every surface) and `daeb94e`
(branch and bound on deals lost, which ended the tail). Every change is
byte-identical to the build before it on 3,000 random combinations and 400 in
exact mode; the oracle agrees on the reference cases and a seeded sweep.
Corpus, 902 runs at 30 s: 902 finish, p50 2.6 ms, p95 94 ms, max 13.6 s
(`KJ9753 / -` exact). `KJ9753 / -` for three tricks went from over fifteen
minutes to 0.74 s. Browser cancel takes 106–109 ms; on the worst case a
bounded status shows by 1.96 s. Open: peak memory rose to 485 MB
(`KJ9753 / -` ev, the `Expected` line memo, which phase 4 removes); the trainer still budgets exact
search as if it took minutes; browser p95 over the corpus is not measured.

### 3. Constraints and coverage for trick targets

Targets only: every item is built, given fixtures and finished for
`Goal::Tricks`. Expected-tricks goals reject each new option until phase 4.
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

Weights after a defender's card are restricted choice, not a bug. Equal
cards are interchangeable, so a layout holding `k` of a class's `m` cards
keeps `C(m, k)` whichever card it showed. Counting only the deals that hold
the very cards seen (`C(m - sW - sE, k - sW)`, which `Solver::post` computes
for pruning) erases restricted choice: tried on 2026-10-18, it scored the
finesse and the drop in `AKT98 / 5432` after East's Q equally (52.2% each,
against 67.3% and 57.7%), and was reverted. `restricted_choice_in_the_game`
in [tests/solver.rs](../tests/solver.rs) pins the hand-derived counts. A
known card breaks its class's symmetry and must become a class of its own;
derive the fixture's weights by hand first.

**Status 2026-10-18:** vacant places and known East/West cards are in
(`Constraints`, `Solver::with_constraints`, `Analysis::total`, CLI
`--vacant`/`--east`/`--west`). A known card is a class of its own; layouts
it rules out drop from the layout set. Checked by five hand-derived
fixtures (two of the first derivations were wrong, and the engine and the
oracle both found the missed lines: a duck against a known Kx, and the drop
when a known Q makes East's Q tell nothing), the policy evaluator under the
same constraints, and the oracle with its own weights on 200 random
constrained combinations. Defaults are byte-identical to before on 3,000
combinations and 400 in exact mode. Expected tricks return
`Error::Unsupported` when constrained.

Length and HCP ranges per defender are in too (`Constraints::length`,
`Constraints::hcp`, CLI `--length`/`--hcp`). HCP means this suit only, a
filter on layouts, not whole-hand modelling. Once HCP are constrained every
missing honour is a class of its own, since which honours a defender holds
stops being symmetric. Checked by three hand-derived fixtures (one HCP range
no known card can express: West holds exactly one of Q, J), the evaluator,
and the oracle sweep, now with random length and HCP ranges; the sweep fails
with honour splitting or the length filter removed. Defaults byte-identical
again on 3,000 combinations and 400 exact. Not yet: the wasm and UI
surfaces (phase 5).

A forced first leader and finite entries are in too (`Constraints::first`,
`Constraints::entries`, CLI `--first`/`--entries`). The rule is SuitPlay's,
for both hands: a hand leads free if it won the last round of the suit, or
leads the first round as the forced leader; otherwise the lead takes one of
its entries. After a defender wins a round they switch suits, so either
hand needs an entry. A problem with no legal first lead (a void forced
leader, no entries anywhere) is `BadConstraints`. `State` carries the hands
that lead free and the entries left; with unlimited entries both stay
constant, so defaults search the same states. Checked by seven
hand-derived fixtures (`AQJT / 32` for four tricks: 18.17% free, 4.84%
with one entry to South, 0.97% with none or when North must lead first;
`AKQ2 / J` blocked, unblocked by one North entry), the evaluator, which
now enforces the entry rule itself, and the oracle sweep with a random
first leader and 0-2 entries per hand; the sweep fails with entries never
spent, a free lead after a defender's trick, or the first leader not
forced. Defaults byte-identical on 3,000 combinations and 400 exact, at the
same speed.

**Done:** each option matches independent target examples; defaults
reproduce phase 1; `AQ / 2` solves; unsupported option combinations,
including a new option with an expected-tricks goal, are rejected clearly.

### 4. Expected tricks as its own game

Taking N tricks and taking the most tricks are different games: the defence
minimizes a different payoff, so an EV plan is a different tree, and EV's
per-layout trick counts are what MP/IMP scoring compares. Build EV on the
exact search, not on target lines.

1. Retire the fast `Expected` mode, the EV line assembled from target
   lines: `line`, `best_line`, `plan_line`, `adopt`, `memo_line`,
   `Game.reqs` and its CLI, wasm and app paths. `ExactExpected` becomes the
   one EV goal. Corpus after phase 2, 122 EV runs each: exact p50 4.5 ms,
   p95 126 ms, max 13.6 s, peak 83 MB; fast p50 32 ms, p95 325 ms, max
   2.1 s, peak 484 MB, unproven on 26, worst miss 0.091 tricks. Cost: past
   its budget (only `KJ9753 / -`, at the browser's 10 s) exact EV shows a
   bound and no line, and the UI says so. This breaks the public API: one
   commit, with the README updated.
2. Extend the phase 3 constraints to EV, each with an EV fixture.
3. SuitPlay scoring: line-versus-line MP and IMP comparison over per-layout
   outcomes. MP is not renamed EV; pairwise preference need not give one
   dominating line, so return the undominated set and represent "no single
   best line". Take SuitPlay's contract and suit-trick inputs and its
   documented IMP convention. First fixtures: `AQ1098 / 5432` (EV vs MP) and
   `AJ98 / K32` (no best MP line).

**Done:** no fast-EV code left; EV fixtures pass on the exact search;
published MP/IMP and cyclic cases agree under matching assumptions; target
fixtures still pass.

### 5. Trainer becomes an analysis tool

Reuse `Plan`, `Lead` and the renderer. Add an Analyse view exposing the
phase 3 constraints and phase 4 scoring, targets first and EV with its
scoring in a tab of its own: all goals at once, goal switching,
a play explorer where the user picks any legal defender reply. Show distinct strategies with per-layout guarantees and a flag when
enumeration is incomplete, a line comparison table, a trick distribution per
complete strategy, and shareable URL/JSON (cards, constraints, goal). Generate
bridge-language summaries from verified trees and keep the tree for audit.

**Done:** reproduce a reference position, inspect the justifying branches,
compare two lines, share it, without playing a random layout first. The
distribution evaluates the same policy as the tree.

### 6. The report

Publish a versioned comparison report: coverage, disagreements, hardware,
native and browser timings, memory, timeouts, proof rates. Claim a tie only
for the measured scope.

## Next step and exclusions

Phase 3, item 3: fixed first plays, finite rounds, specified discards and
ruffs for trick targets.

Not needed: rewrite, server backend, new framework, GPU search, full-deal AI,
bidding inference. Mixed-strategy equilibria are a separate research question.
