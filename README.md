# suit-combination

See the [calculator comparison](docs/suit-calculator-research.md) and
[plan to match the leading tools](docs/parity-plan.md) for the researched roadmap.

Single-dummy suit-combination solver for contract bridge, plus a browser
trainer built on it.  Given North's and South's cards in one suit, it finds
the line of play that is best for at least *N* tricks against defenders who
see all four hands, and the best line at matchpoints or IMPs.

The search is exact: each information state carries the Pareto antichain of
"tricks per layout" vectors some continuation guarantees, so the answer is the
optimum over declarer strategies, not a greedy per-branch estimate (which is
wrong whenever a falsecard spreads one layout over several branches).
Layouts are weighted by vacant places, so every probability is an exact
fraction over C(26, 13).

Expected tricks is not a goal: it is neither matchpoints nor IMPs, so it
says nothing about a real board.  Lines are compared under a scoring
instead (`Solver::compare`).

Assumptions, as in Pavlicek's card-combination tables: unlimited entries to
both hands, no ruffs, no inference about the enemy hands, only tricks in this
suit count.  Any number of missing cards: the limit is 256 layouts once
interchangeable cards merge, which every holding meets (at most 162) and only
many known cards or an HCP range can break.  `Constraints` set other vacant
places, cards known to be with East or West, length and HCP
ranges, the hand the lead starts in, finite entries, a limit on rounds, a
fixed first card, discards and ruffs.

## Library

```rust
use contract_bridge::Holding;
use suit_combination::analyze;

let north: Holding = "AJ52".parse().unwrap();
let south: Holding = "K76".parse().unwrap();
let a = analyze(north, south, 3).unwrap();
println!("{:.1}%\n{}", a.value() * 100.0, a.plan); // 77.1%, then the line
```

`analyze_all` solves every target in one pass.
`Solver` keeps the memo across goals, and `Game` plays the suit out on it
trick by trick: `moves` scores each card declarer may play now, and `play`
plays one and lets the engine's defenders answer.  The `serde` feature derives
`Serialize` for the result types.

`Solver::with_constraints` takes `Constraints`: the defenders' vacant places
(13 less the cards known in their other suits), the cards of this suit
known to be with each, and ranges of each one's length and high-card points
in this suit (not the whole hand), the hand the lead starts in, and
entries to North and South in other suits.  A hand leads free after winning
the last round of the suit, or for the first round from the hand the lead
starts in (either, if none is named); otherwise the lead takes
one of its entries, as in SuitPlay (the defenders, after winning a round,
switch suits).  They also take a limit on the rounds played (tricks count
only within them), a card declarer must lead first, and discards: declarer's
cards spent in other suits, out of play but not given to the defenders, as
SuitPlay's "discards or ruffs".  And ruffs, as SuitC's: the rounds each
seat may ruff once void in the suit.  A ruff is optional, beats every card of
the suit and counts as a trick in it; only one side may ruff, as trump ranks
are not modelled (no overruff).  Every goal takes them, and scores are
then out of `Analysis::total`, not `TOTAL`.  The CLI takes `--vacant EAST,WEST`,
`--east CARDS`, `--west CARDS`, `--length EAST,WEST` and `--hcp EAST,WEST`,
ranges such as `-,-1` (West at most one card) or `-,1-2`, `--first N|S`
and `--entries NORTH,SOUTH` such as `-,1` (`-` for unlimited), `--rounds N`,
`--lead CARD`, `--discard CARDS` and `--ruffs N,E,S,W`.

`Solver::compare` scores lines against each other, as SuitPlay does:
`Scoring::Matchpoints` counts how often one line takes more tricks than
another, whatever the margin; `Scoring::Imps { contract, vulnerable, need }`
counts the IMPs won (WBF scale) in a contract made with `need` tricks in the
suit.  There can be millions of lines no other line beats on every layout
(`KJ9753` opposite a void), so it does not list them.  It returns the best
line, then for each other first card the best line found with it, each
with its trick distribution, tricks per layout, first card and plan, and
`Comparison::gain(a, b)` for what one line gains over another.
`proven` says no line at all beats the first: the narrow search's lines
after each first card start, the one beating or tying most of them is
checked against every line, and is replaced by what beats it most until
nothing does.  That check keeps, per layout, only whether a line takes
fewer, as many or more tricks than the line checked (at IMPs, where the
IMPs change), and drops any line that cannot beat it even taking
double-dummy tricks everywhere else.  An interrupted check leaves the
comparison `open`; asking again goes on.  Preference can go round in a
cycle (`AJ98 / K32` at matchpoints): `best` then lists the lines found
that beat each other.  `Solver::choices` weighs every card declarer may
play at a position of a line, as a chess engine's analysis board weighs
moves: the line with that card and the best way on from it in place of its
own, against the line, out of every deal, the defenders still playing as
they would against the line elsewhere.  After a proven best line no card
is worth more than the line's own.  Lines
treat cards declarer cannot yet tell apart alike, as the rest of the engine
does.  That loses nothing for a target, but at MP or IMPs a line telling
them apart can take a trick more on a layout:
`32 / KT864`, reacting to East's 7 and 9 differently once the 8 is gone.
The CLI takes `mp` and `imps CONTRACT NEED [--vulnerable]`, e.g.
`solve Q543 AT2 imps 3N 2 --vulnerable`.

A search can be given a budget.  `Solver::set_interrupt` takes a callback
that is polled every 1024 or so states searched, with the number of states
memoized; once it returns `true`, `analyze` fails with
`Error::Interrupted { bound, total }`.  No line is known then, but none scores more
than `bound`: the target's score where its search finished, else what
declarer would make seeing all four hands (double dummy); a comparison's
bound is that summed over every target.  The memo keeps only finished
work, so asking again on
the same `Solver` goes on from where it stopped, and gives exactly what an
uninterrupted search gives.  The library has no clock: the caller's callback
reads one.  `Game` never polls it.

```rust
use std::time::{Duration, Instant};
use suit_combination::{Error, Solver};

let mut solver = Solver::new("KQ85".parse().unwrap(), "T73".parse().unwrap()).unwrap();
let deadline = Instant::now() + Duration::from_secs(1);
solver.set_interrupt(move |_states| Instant::now() >= deadline);
match solver.analyze(2) {
    Ok(a) => println!("{:.1}%", a.value() * 100.0),
    Err(Error::Interrupted { bound, .. }) => println!("at most {bound} of 10400600"),
    Err(e) => panic!("{e}"),
}
```

Polls fall between states, so a state whose antichains take seconds to
combine (the hardest cases below) can overrun a budget by that much; kill
the thread or worker for a hard stop.

```sh
cargo run --release --example solve -- AJ52 K76      # all goals
cargo run --release --example solve -- AQ3 42 2      # one target
cargo run --release --example solve -- AQT98 5432 mp  # the best line at matchpoints
cargo run --release --example solve -- KQ85 T73 --budget 1  # each goal 1 s
```

The CLI prints each goal's value, or `interrupted (best <= x)` when the
budget ran out.
Bad holdings and unsupported problems (more than 256 layouts) exit with
status 2 and a message.

## Evidence

[`tests/fixtures/cases.json`](tests/fixtures/cases.json) holds reference cases
with their sources; [`tests/fixtures.rs`](tests/fixtures.rs) checks them and
replays every returned plan on every layout against every legal defence.
[`examples/bench.rs`](examples/bench.rs) runs the seeded corpus in
[`tests/fixtures/corpus.txt`](tests/fixtures/corpus.txt), one process per
goal with the timeout as its budget, and reports timings, memory and how
each run ended (`proven`, `interrupted` with its bound, or `timeout` when
killed at twice the budget):

```sh
cargo run --release --example bench -- run tests/fixtures/corpus.txt 30 > bench.tsv
```

## Web trainer

[`web/`](web/) is a wasm front end: it deals a random combination, picks a goal
worth thinking about, and plays the suit out against you.  Every card you
choose is graded against the best play from the same information; the
defenders hold a random layout (vacant-places odds) and answer with the card
that hurts most on it, breaking ties by the most deceptive one.  At the end it
shows every lead's value and the line.  The line need not play every card
that is best afresh from here: it keeps what the optimum needs over every
layout, while a falsecarding defender makes the layouts still possible
unequally likely to reach here.  So the trainer grades the line's own card
right too.

`analyse.html` analyses one combination without playing it: cards and
every `Constraints` field, all trick targets at once, every first card,
each line in words, as a tree, and as an explorer where you pick any
defender reply, and what it guarantees on every layout.  A tab of its own
has the best line at matchpoints or IMPs (`Solver::compare`), proven or
the best found so far, and the best with each other first card: trick
distribution, gains against a chosen line, the cycle if there is one,
what each card declarer may play is worth at a position of the explorer
(click declarer's card there), and two lines
side by side layout by layout.  The problem lives in the URL hash as JSON,
so "Copy link" shares it; "JSON" downloads it with every result.  The bridge
words (`web/render.js`, checked by `node web/render.test.mjs`) come from
the verified tree and name only what the cards show: a lead, a cash, a
finesse, a duck, letting a card run.

Every goal shows its value as it arrives, or interrupted with the bound no
line beats.  The worker answers in slices of 300 ms
(`Trainer::set_budget`, `Trainer::target(k)`, `Trainer::compare(scoring)`;
an interrupted call returns `{interrupted: true, bound}` and keeps its memo),
so a combination you enter solves the goal you ask for first, then the
others, showing the time spent meanwhile.  Play starts once your goal is done
and every other goal has had a turn.  A search runs ten seconds at a time;
"Keep searching" goes on in the same worker from where it stopped.  Cancel stops at once, keeping every
result so far; if the slice in flight does not end within 100 ms it
terminates the worker, whose memo is then lost.  A goal whose search fills
1 GiB of wasm memory stops as "out of memory" and is not tried again; one
state can still take gigabytes at once (the exact search on `KQ85` opposite
`T73`), and then the worker crashes and restarts, again keeping every result
so far, and play goes on with the goal you asked for if it had finished.

```sh
cd web && wasm-pack build --release --target web && python3 -m http.server 8765
```

Deployed to GitHub Pages by [`.github/workflows/pages.yml`](.github/workflows/pages.yml).
`web/.cargo/config.toml` pins the wasm target features because a global
`target-cpu=native` otherwise breaks wasm-bindgen.

## Limits

Random 7–9 card problems solve in a few milliseconds.  The Pareto search is
exponential in the worst case.  For a trick target the vectors are collapsed
to 0/1 ("at least that many more tricks on this layout"), which keeps the
antichains small.  Only the best vector counts in the end, so a narrow
search first finds a good line, and the exact search then drops every
vector that already loses more deals than that line.  The hardest
combinations known, such as `KQ85` opposite `T73` and `KJ9753` opposite a
void, take under a second per target.  `Solver::compare` proves the best
line on `KJ9753` opposite the 2 (3,880 undominated lines) in 0.3 s and on
`KQ85` opposite `T73` in 0.15 s; `KJ97` opposite `53` ends in a cycle of
lines in 4 s, `KJ9753` opposite a void in a cycle of nine in 8 s.  At the
first trick, where a defender's cards multiply the ways to compare into
hundreds of thousands, the check searches them depth first instead, cut
by a bound on the gain still possible.  The web trainer runs the solver in a worker, so
the page stays responsive; a random deal is abandoned after two seconds and
another dealt, and a combination you enter yourself searches ten seconds at a
time until it finishes or you stop it.  When a search is interrupted its bound comes from
double dummy, which costs at most about 20 ms (`KJ9753` opposite a void).
