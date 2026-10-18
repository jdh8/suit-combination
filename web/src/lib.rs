//! Wasm binding for the suit-combination web UI: one [`Trainer`] per
//! combination holds the memoized solver, answers the analysis of each goal
//! as JSON, and plays the suit out trick by trick.

use contract_bridge::Holding;
use suit_combination::{Comparison, Constraints, Error, Game, Scoring, Solver};
use wasm_bindgen::prelude::{JsValue, wasm_bindgen};

#[wasm_bindgen]
extern "C" {
    /// `Date.now()`: wasm32-unknown-unknown has no clock of its own
    #[wasm_bindgen(js_namespace = Date)]
    fn now() -> f64;
}

/// Bytes of wasm memory past which an analysis stops, keeping its memo: an
/// allocation that fails traps and loses it all (Chrome traps at about
/// 2.3 GiB).
// ponytail: checked between states, so a memo that grows gradually stops
// here, but one state's combine can still take gigabytes at once (the exact
// search on KQ85 opposite T73); then the worker crashes and app.js recovers.
const MEMORY_CAP: usize = 1 << 30;

/// Bytes of wasm memory in use (it never shrinks)
fn memory() -> usize {
    #[cfg(target_arch = "wasm32")]
    return core::arch::wasm32::memory_size(0) << 16;
    #[cfg(not(target_arch = "wasm32"))]
    0
}

#[wasm_bindgen]
pub struct Trainer {
    solver: Solver,
    game: Option<Game>,
    /// The last comparison, whose lines [`line`](Self::line) plays out
    comparison: Option<Comparison>,
    max_tricks: u8,
    /// Milliseconds each analysis may take, if limited
    budget: Option<f64>,
}

// A JsValue error (rather than a String) keeps wasm-bindgen's externref
// table in the binary; without any JsValue the CLI fails to find it.
fn err(e: impl ToString) -> JsValue {
    JsValue::from_str(&e.to_string())
}

#[wasm_bindgen]
impl Trainer {
    /// `north` opposite `south`, ranks in descending order, e.g. `AJ52`,
    /// under `constraints`, `suit_combination::Constraints` as JSON (any
    /// field left out keeps its default)
    #[wasm_bindgen(constructor)]
    pub fn new(north: &str, south: &str, constraints: Option<String>) -> Result<Trainer, JsValue> {
        let north: Holding = north.parse().map_err(|e| err(format!("North: {e}")))?;
        let south: Holding = south.parse().map_err(|e| err(format!("South: {e}")))?;
        let constraints: Constraints = match constraints {
            Some(json) => {
                serde_json::from_str(&json).map_err(|e| err(format!("constraints: {e}")))?
            }
            None => Constraints::default(),
        };
        let solver = Solver::with_constraints(north, south, constraints).map_err(err)?;
        Ok(Self {
            max_tricks: solver.max_tricks(),
            solver,
            game: None,
            comparison: None,
            budget: None,
        })
    }

    /// Every layout of the missing cards, `suit_combination::Layout`s as
    /// JSON, in the order of each guarantee
    pub fn layouts(&mut self) -> String {
        serde_json::to_string(&self.solver.layouts()).unwrap_or_default()
    }

    /// `suit_combination::Comparison` under `scoring`
    /// (`suit_combination::Scoring` as JSON), or, out of budget or memory,
    /// `{interrupted: true, bound, memory}`; the bound is not meaningful here
    pub fn compare(&mut self, scoring: &str) -> Result<String, JsValue> {
        let scoring: Scoring =
            serde_json::from_str(scoring).map_err(|e| err(format!("scoring: {e}")))?;
        self.arm();
        match self.solver.compare(scoring) {
            Ok(c) => {
                let json = serde_json::to_string(&c).map_err(err);
                self.comparison = Some(c);
                json
            }
            Err(e) => Ok(self.interrupted(e)?.to_string()),
        }
    }

    /// The `suit_combination::Plan` of line `i` of the last comparison, as
    /// JSON
    pub fn line(&mut self, i: usize) -> Result<String, JsValue> {
        let line = (self.comparison.as_ref())
            .and_then(|c| c.lines.get(i))
            .ok_or_else(|| err("no such line"))?;
        serde_json::to_string(&self.solver.line(line)).map_err(err)
    }

    /// Stop the next search at the budget or the memory cap
    fn arm(&mut self) {
        let deadline = self.budget.map(|ms| now() + ms);
        self.solver
            .set_interrupt(move |_| memory() > MEMORY_CAP || deadline.is_some_and(|d| now() >= d));
    }

    /// `{interrupted: true, bound, memory}` for an interrupted search
    fn interrupted(&self, e: Error) -> Result<serde_json::Value, JsValue> {
        match e {
            Error::Interrupted { bound, .. } => Ok(serde_json::json!({
                "interrupted": true,
                "bound": bound,
                "memory": memory() > MEMORY_CAP,
            })),
            e => Err(err(e)),
        }
    }

    /// Give each later analysis at most `ms` milliseconds (0: no limit).
    /// The memo keeps what an interrupted one finished, so asking again
    /// goes on from there.
    pub fn set_budget(&mut self, ms: f64) {
        self.budget = (ms > 0.0).then_some(ms);
    }

    /// The `suit_combination::Analysis` of `target`, or, out of budget or
    /// memory, `{interrupted: true, bound, memory}` with an upper bound
    /// on its score; `memory` says asking again is no use
    fn analyze(&mut self, target: u8) -> Result<serde_json::Value, JsValue> {
        self.arm();
        match self.solver.analyze(target) {
            Ok(a) => serde_json::to_value(&a).map_err(err),
            Err(e) => self.interrupted(e),
        }
    }

    /// [`analyze`](Self::analyze) for at least `target` tricks, as JSON
    pub fn target(&mut self, target: u8) -> Result<String, JsValue> {
        Ok(self.analyze(target)?.to_string())
    }

    /// Every target from one trick up to the most there are: a JSON
    /// array of what [`target`](Self::target) returns, each within the budget
    pub fn targets(&mut self) -> Result<String, JsValue> {
        let all = (1..=self.max_tricks)
            .map(|target| self.analyze(target))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(serde_json::Value::from(all).to_string())
    }

    /// Deal `west` the given cards (East gets the rest) and start playing
    /// for `target` tricks. Returns the opening snapshot. Play runs to the
    /// end of each call, whatever the budget, on what the analysis memoized.
    pub fn start(&mut self, west: &str, target: u8) -> Result<String, JsValue> {
        let west: Holding = west.parse().map_err(|e| err(format!("West: {e}")))?;
        self.game = Some(Game::new(&mut self.solver, west, target).map_err(err)?);
        self.snapshot(vec![])
    }

    /// Declarer plays `rank`; the defenders (and any forced declarer cards)
    /// follow.  Returns the snapshot after them, with every card played.
    pub fn play(&mut self, rank: &str) -> Result<String, JsValue> {
        let rank = rank.parse().map_err(err)?;
        let game = self.game.as_mut().ok_or_else(|| err("no game"))?;
        let plays = game.play(&mut self.solver, rank).map_err(err)?;
        self.snapshot(plays)
    }

    fn snapshot(&mut self, plays: Vec<suit_combination::Play>) -> Result<String, JsValue> {
        let game = self.game.as_ref().ok_or_else(|| err("no game"))?;
        Ok(serde_json::json!({
            "north": game.north(),
            "south": game.south(),
            "won": game.tricks_won(),
            "over": game.is_over(),
            "weight": game.weight(&mut self.solver),
            "moves": game.moves(&mut self.solver),
            "plays": plays,
        })
        .to_string())
    }
}
