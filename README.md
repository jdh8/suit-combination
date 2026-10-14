# suit-combination

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

`analyze_all` solves every target and the expected-tricks goal in one pass.
`Solver` keeps the memo across goals, and `Game` plays the suit out on it
trick by trick: `moves` scores each card declarer may play now, and `play`
plays one and lets the engine's defenders answer.  The `serde` feature derives
`Serialize` for the result types.

```sh
cargo run --release --example solve -- AJ52 K76      # all goals
cargo run --release --example solve -- AQ3 42 2      # one target
cargo run --release --example solve -- A943 QT2 ev   # expected tricks
```

## Web trainer

[`web/`](web/) is a wasm front end: it deals a random combination, picks a goal
worth thinking about, and plays the suit out against you.  Every card you
choose is graded against the best play from the same information; the
defenders hold a random layout (vacant-places odds) and answer with the card
that hurts most on it, breaking ties by the most deceptive one.  At the end it
shows every lead's value and the optimal line.

```sh
cd web && wasm-pack build --release --target web && python3 -m http.server 8765
```

Deployed to GitHub Pages by [`.github/workflows/pages.yml`](.github/workflows/pages.yml).
`web/.cargo/config.toml` pins the wasm target features because a global
`target-cpu=native` otherwise breaks wasm-bindgen.

## Limits

Random 7–9 card problems solve in a few milliseconds.  The Pareto search is
exponential in the worst case: a combination with many separately-ranked
spot cards (`A862` opposite `J94`) takes a few seconds for the expected-tricks
goal, during which the page is unresponsive.
