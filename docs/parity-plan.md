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
| Expected tricks | `ExactExpected`, the exact search, is the one EV goal (the fast target-line mode was retired in phase 4) | No MP/IMP |
| Probabilities | Fixed 13/13 vacant places, `TOTAL = C(26,13)` hard-coded in engine and browser | No vacant places, known cards, length or HCP constraints |
| Entries and play | Unlimited entries, no ruffs by default; the starting hand, finite entries, round limits, a fixed first card, discards and one side's ruffs (`Constraints`) | No overruffs |
| Alternative lines | Values for every equivalent opening lead; one plan per goal | No line browser, line-by-layout matrix or comparison |
| Goals | At-least-N, exact EV | No MP/IMP line comparison |
| Coverage | Any holding: the limit is 256 layouts after merging interchangeable cards (at most 162 for any holding); only many known cards or an HCP range exceed it (`TooManyMissing`) | Over-limit constraint combinations are rejected, not solved |
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
| `AQ / 2`, >=2 | `TooManyMissing` | panic in the example CLI (2026-10-18: 50.00%, 76 µs) |

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

The hand the lead starts in and finite entries are in too
(`Constraints::first`, `Constraints::entries`, CLI `--first`/`--entries`).
A hand leads free if it won the last round of the suit, or leads the first
round from where the lead starts (either hand if none is named); otherwise
the lead takes one of its entries. After a defender wins a round they
switch suits, so either hand needs an entry. This is SuitPlay's rule for
both hands, and SuitC's and CCAnalyser's "on lead": not a forced leader,
only "relevant if entries are limited" (CCAnalyser), and with no hand on
lead SuitC lets either lead first free. A problem with no legal first lead
(a void starting hand and no entry to the other) is `BadConstraints`.
`State` carries the hands that lead free and the entries left; with
unlimited entries both stay constant, so defaults search the same states.
Checked by CCAnalyser's published entry examples (`AQJT / 65432`, 8/11
vacant places, lead in South, two South entries: 40.30%, and 95.71% with
East at 0 HCP; its per-lead values agree too), hand derivations (that
holding with one to three South entries; `AQJT / 32`, `AQJ / 432`,
`AQ / 432`, `AKQ2 / 543`; `AKQ2 / J` blocked, unblocked by one North entry),
the evaluator, which enforces the entry rule itself, and the oracle sweep
with a random starting hand and 0-2 entries per hand; the sweep fails with
entries never spent, a free lead after a defender's trick, an unnamed first
lead charged, or a named one ignored. SuitC's probe values for `AJT / 432`
(32.96%, 76.00%) agree but are not fixtures. Defaults byte-identical on
3,000 combinations and 400 exact, at the same speed.

Item 3 so far: a limit on rounds (SuitC's trick limit: tricks count only
within it), a fixed first card (SuitC's "lead card"; `Analysis::leads`
already scores every first card, this also gives the line after one), and
discards (SuitPlay's: declarer's card leaves play and is not the
defenders'), as `Constraints::rounds`, `lead`, `discards` and CLI
`--rounds`/`--lead`/`--discard`. Checked by hand derivations (SuitC
probes agreed on the round limits), CCAnalyser's published first-card
values for `AQJT / 65432` (N:A 12.43% and 12.49% at 8/11, N:Q 0.00%), the
evaluator enforcing each option, and the oracle sweep with random limits,
discards and first cards; mutants of each are caught. Defaults
byte-identical, same speed.

Ruffs complete item 3 (`Constraints::ruffs`, CLI `--ruffs N,E,S,W`):
SuitC's per-seat count of rounds a player may ruff once void, with the
semantics SuitC leaves undocumented now fixed. A ruff is optional, beats
every card of the suit and counts as a trick in it; the winner of a
ruffed round leads next as with any other (entries permitting). Only one
side may ruff (`Error::Unsupported` otherwise): trump ranks are not
modelled, so there is no overruff, and SuitC's author warns that mixing
sides "leads to unpredictable effects". Declarer sees a defender's ruff as
its own branch (`Plan::Defender::ruff`) and plays one as `Plan::Ruff`. An
interrupted search with declarer ruffs bounds by every trick, as double
dummy does not see them. `Game` rejects ruffs until the analysis view can
show them. Checked by hand derivations that SuitC's probes matched
(`AKQ / 432` with East or both defenders ruffing, `A2 / KQ543` with a
North ruff), the SuitC guide's published `KQT32 / 654` table with a ruff
each (63.00%), the evaluator, and the oracle sweep with random ruffs on
one side; mutants (no defender ruff, no declarer ruff, ruffs unspent, a
ruff that loses) are caught. Defaults byte-identical, same speed.

Item 4: the limit is now 256 layouts after interchangeable cards merge,
not eight missing cards (`MAX_LAYOUTS` replaces `MAX_MISSING`). Every
holding fits: twelve missing cards make at most 162 layouts, as declarer's
few cards separate few classes. Only known cards or an HCP range, which
make cards classes of their own, can pass 256; those problems, and no card
held at all, are `TooManyMissing`. The root has the finest layout space,
so it alone is checked, and 256 layouts allow at most 8 classes, so
`State` keeps its size. `AQ / 2` solves (50.00%, 76 µs). Checked by
hand-derived fixtures with 9 to 13 missing or none (`AQ / 2`, `AKJ / 2`,
`K2 / 3`, `AKQ / -`, `A / -`, all thirteen cards), the oracle on the
reference cases without the old skip, and a new sweep of 240 random
combinations with 9 to 12 missing, half under random constraints other
than known cards and HCP (most of those pass 256 layouts); the oracle
skips 5 whose antichains pass 2,000 vectors. The corpus generator now
covers 4 to 12 missing (31 new combinations; old lines unchanged): 138
runs, p50 0.2 ms, p95 2.5 ms, max 16 ms, all proven. Defaults with at
most eight missing are byte-identical.

**Status:** phase 3 done.

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

**Status 2026-10-18:** item 1 done.  `Goal::Expected`, its line search
(`line`, `best_line`, `plan_line`, `adopt`, `memo_line`, `Game.reqs`),
`Analysis::bound`/`proven` and `Lead::bound` (always equal to the score
without it) are gone; `analyze_all`, the CLI's `ev`, the bench's `ev`, the
wasm `expected()` and the trainer's EV all run `ExactExpected`.  Targets
and exact EV are byte-identical to before on 3,000 combinations and 400 in
exact mode.  Corpus at 30 s, 887 runs: all finish, p50 1.4 ms, p95 25.5 ms,
max 12.2 s (`KJ9753 / -` ev), peak memory 86 MB (was 485 MB).  Found on
the way, and not new: a plan card can grade below best in the `Game` (the
plan keeps what the root's optimum needs; the game grades by the best from
the current information): 793 target and 1,128 EV cards of 84,199 on 150
random combinations, in 51 of them.  Only the fast line, greedy by
construction, never did.  Phase 5 decides whether the plan breaks ties
toward the locally best card or the trainer grades by the plan.

Item 2 done too: every phase 3 constraint takes expected tricks.  The
exact search needed no change, only the gate (`Solver::check`) lifted:
its loss bound `cap` stays at least the tricks left under round limits,
discards and ruffs, so the pruning stays sound.  `analyze_all` and the
CLI's default include expected tricks under constraints (the CLI's target
range now also stops at the rounds and discards).  Checked by ten EV
fixtures, one per option: nine hand derivations, each a line optimal for
every target at once, so the expected tricks are the sum of the targets'
optima (vacant places, a known card, HCP, length, first hand and entries,
rounds, a fixed first card, a discard, a defender's ruff), and the SuitC
guide's ruff table (`KQT32 / 654`, a ruff each: 1.6300); by the policy
evaluator on every constrained fixture; and by the oracle sweep, now with
expected tricks under every random constraint (a mutant off by one deal
under ruffs fails it).  Defaults byte-identical on 3,000 combinations.

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

Phase 4, item 3: SuitPlay's MP and IMP line comparison.

Not needed: rewrite, server backend, new framework, GPU search, full-deal AI,
bidding inference. Mixed-strategy equilibria are a separate research question.
