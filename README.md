# suit-combination

See the [calculator comparison](docs/suit-calculator-research.md) and
[plan to match the leading tools](docs/parity-plan.md) for the researched roadmap.

Single-dummy suit-combination solver for contract bridge, plus a browser
trainer built on it.  Given North's and South's cards in one suit, it finds
the line of play that is best for a goal — at least *N* tricks, or the most
tricks on average — against defenders who see all four hands.

The search is exact: each information state carries the Pareto antichain of
"tricks per layout" vectors some continuation guarantees, so the answer is the
optimum over declarer strategies, not a greedy per-branch estimate (which is
wrong whenever a falsecard spreads one layout over several branches).
Layouts are weighted by vacant places, so every probability is an exact
fraction over C(26, 13).

The most tricks on average come two ways.  `Goal::Expected` combines the
targets' optimal lines: it follows one of them, and where it is made or
hopeless adopts whichever target's line from there scores best.  It is fast
and comes with a bound, the sum of the targets' success probabilities, that
no line can beat.  On 600 random combinations the line was the best on 95%,
proven so by the bound on 81%, and at worst 0.06 tricks short.
`Goal::ExactExpected` searches the full "tricks per layout" vectors instead,
which is exact but can take seconds.

Assumptions, as in Pavlicek's card-combination tables: unlimited entries to
both hands, no ruffs, no inference about the enemy hands, only tricks in this
suit count.  Up to 8 missing cards.  For trick targets, `Constraints` set
other vacant places, cards known to be with East or West, length and HCP
ranges, the hand that leads first and finite entries.

## Library

```rust
use contract_bridge::Holding;
use suit_combination::{analyze, Goal};

let north: Holding = "AJ52".parse().unwrap();
let south: Holding = "K76".parse().unwrap();
let a = analyze(north, south, Goal::Tricks { target: 3 }).unwrap();
println!("{:.1}%\n{}", a.value() * 100.0, a.plan); // 77.1%, then the line
```

`analyze_all` solves every target and `Goal::Expected` in one pass.
`Solver` keeps the memo across goals, and `Game` plays the suit out on it
trick by trick: `moves` scores each card declarer may play now, and `play`
plays one and lets the engine's defenders answer.  The `serde` feature derives
`Serialize` for the result types.

`Solver::with_constraints` takes `Constraints`: the defenders' vacant places
(13 less the cards known in their other suits), the cards of this suit
known to be with each, and ranges of each one's length and high-card points
in this suit (not the whole hand), the hand that must lead first, and
entries to North and South in other suits.  A hand leads free after winning
the last round of the suit, or as the first leader; otherwise the lead takes
one of its entries, as in SuitPlay (the defenders, after winning a round,
switch suits).  Only trick targets take them
(`Error::Unsupported` otherwise), and scores are then out of
`Analysis::total`, not `TOTAL`.  The CLI takes `--vacant EAST,WEST`,
`--east CARDS`, `--west CARDS`, `--length EAST,WEST` and `--hcp EAST,WEST`,
ranges such as `-,-1` (West at most one card) or `-,1-2`, `--first N|S`
and `--entries NORTH,SOUTH` such as `-,1` (`-` for unlimited).

Every `Analysis` says whether it is `proven`: its `score` equals its `bound`,
so no line does better.  Only a `Goal::Expected` line can fall short; the
best then lies in `score..=bound`, and each `Lead` carries its own `bound`
for the lines starting with it (the global bound does not certify the other
leads).

A search can be given a budget.  `Solver::set_interrupt` takes a callback
that is polled every 1024 or so states searched, with the number of states
memoized; once it returns `true`, `analyze` fails with
`Error::Interrupted { bound, total }`.  No line is known then, but none scores more
than `bound`: each target's score where its search finished, else what
declarer would make seeing all four hands (double dummy), summed over targets
for expected tricks.  The memo keeps only finished work, so asking again on
the same `Solver` goes on from where it stopped, and gives exactly what an
uninterrupted search gives.  The library has no clock: the caller's callback
reads one.  `Game` never polls it.

```rust
use std::time::{Duration, Instant};
use suit_combination::{Error, Goal, Solver};

let mut solver = Solver::new("KQ85".parse().unwrap(), "T73".parse().unwrap()).unwrap();
let deadline = Instant::now() + Duration::from_secs(1);
solver.set_interrupt(move |_states| Instant::now() >= deadline);
match solver.analyze(Goal::Tricks { target: 2 }) {
    Ok(a) => println!("{:.1}%", a.value() * 100.0),
    Err(Error::Interrupted { bound }) => println!("at most {bound} of 10400600"),
    Err(e) => panic!("{e}"),
}
```

Polls fall between states, so a state whose antichains take seconds to
combine (the hardest cases below) can overrun a budget by that much; kill
the thread or worker for a hard stop.

```sh
cargo run --release --example solve -- AJ52 K76      # all goals
cargo run --release --example solve -- AQ3 42 2      # one target
cargo run --release --example solve -- A943 QT2 ev     # expected tricks
cargo run --release --example solve -- A943 QT2 exact  # ... by the exact search
cargo run --release --example solve -- KQ85 T73 --budget 1  # each goal 1 s
```

The CLI prints each goal as `proven`, as `best <= x` when the line found is
not proven best, or as `interrupted (best <= x)` when the budget ran out.
Bad holdings and unsupported problems (more than 8 cards missing) exit with
status 2 and a message.

## Evidence

[`tests/fixtures/cases.json`](tests/fixtures/cases.json) holds reference cases
with their sources; [`tests/fixtures.rs`](tests/fixtures.rs) checks them and
replays every returned plan on every layout against every legal defence.
[`examples/bench.rs`](examples/bench.rs) runs the seeded corpus in
[`tests/fixtures/corpus.txt`](tests/fixtures/corpus.txt), one process per
goal with the timeout as its budget, and reports timings, memory, proof
rates and how each run ended (`proven`, `bounded`, `interrupted` with its
bound, or `timeout` when killed at twice the budget):

```sh
cargo run --release --example bench -- run tests/fixtures/corpus.txt 30 > bench.tsv
```

## Web trainer

[`web/`](web/) is a wasm front end: it deals a random combination, picks a goal
worth thinking about, and plays the suit out against you.  Every card you
choose is graded against the best play from the same information; the
defenders hold a random layout (vacant-places odds) and answer with the card
that hurts most on it, breaking ties by the most deceptive one.  At the end it
shows every lead's value and the line.  For expected tricks it runs the
exact search only when the bound does not already prove the combined line
best, and keeps that line, marked as such, if the exact search runs out of
time.

Every goal shows its status as it arrives: proven, best found (with the bound
no line beats), or interrupted (with that bound only); nothing unproven is
called best or optimal.  The worker answers in slices of 300 ms
(`Trainer::set_budget`, `Trainer::target(k)`, `Trainer::expected(exact)`;
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
void, take under a second per target, and `Goal::Expected` adds a second or
two at most.  `Goal::ExactExpected` needs the full vectors: about a second
on those, but about fifteen seconds on `KJ9753` opposite a void.  The web
trainer runs the solver in a worker, so
the page stays responsive; a random deal is abandoned after two seconds and
another dealt, and a combination you enter yourself searches ten seconds at a
time until it finishes or you stop it.  When a search is interrupted its bound comes from
double dummy, which costs at most about 20 ms (`KJ9753` opposite a void).
