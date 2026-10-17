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
which is exact but can take minutes.

Assumptions, as in Pavlicek's card-combination tables: unlimited entries to
both hands, no ruffs, no inference about the enemy hands, only tricks in this
suit count.  Up to 8 missing cards.

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

```sh
cargo run --release --example solve -- AJ52 K76      # all goals
cargo run --release --example solve -- AQ3 42 2      # one target
cargo run --release --example solve -- A943 QT2 ev     # expected tricks
cargo run --release --example solve -- A943 QT2 exact  # ... by the exact search
```

## Evidence

[`tests/fixtures/cases.json`](tests/fixtures/cases.json) holds reference cases
with their sources; [`tests/fixtures.rs`](tests/fixtures.rs) checks them and
replays every returned plan on every layout against every legal defence.
[`examples/bench.rs`](examples/bench.rs) runs the seeded corpus in
[`tests/fixtures/corpus.txt`](tests/fixtures/corpus.txt), one process per
goal with a timeout, and reports timings, memory, timeouts and proof rates:

```sh
cargo run --release --example bench -- run tests/fixtures/corpus.txt 30 > bench.tsv
```

## Web trainer

[`web/`](web/) is a wasm front end: it deals a random combination, picks a goal
worth thinking about, and plays the suit out against you.  Every card you
choose is graded against the best play from the same information; the
defenders hold a random layout (vacant-places odds) and answer with the card
that hurts most on it, breaking ties by the most deceptive one.  At the end it
shows every lead's value and the optimal line.  For expected tricks it runs the
exact search only when the bound does not already prove the combined line
best, and keeps that line, marked as such, if the exact search runs out of
time.

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
antichains small: hard combinations such as `A953` opposite `KJ8` and
`KJ84` opposite `T63` take well under a second per target and `KQ85`
opposite `T73` about ten seconds, but for three tricks the hardest known,
`KJ9753` opposite a void, does not finish in fifteen minutes.
`Goal::Expected` adds well under a second to the targets, while
`Goal::ExactExpected` needs the full vectors and takes about twenty seconds
on `A953` opposite `KJ8`.  The web trainer runs the solver in a worker, so
the page stays responsive; a random deal is abandoned after two seconds and
another dealt, and a combination you enter yourself runs until it finishes
or you cancel it.
