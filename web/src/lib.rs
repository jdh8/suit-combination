//! Wasm binding for the suit-combination web UI: one [`Trainer`] per
//! combination holds the memoized solver, answers the analysis of each goal
//! as JSON, and plays the suit out trick by trick.

use contract_bridge::Holding;
use suit_combination::{Error, Game, Goal, Solver};
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
    /// `north` opposite `south`, ranks in descending order, e.g. `AJ52`
    #[wasm_bindgen(constructor)]
    pub fn new(north: &str, south: &str) -> Result<Trainer, JsValue> {
        let north: Holding = north.parse().map_err(|e| err(format!("North: {e}")))?;
        let south: Holding = south.parse().map_err(|e| err(format!("South: {e}")))?;
        Ok(Self {
            solver: Solver::new(north, south).map_err(err)?,
            game: None,
            max_tricks: north.len().max(south.len()) as u8,
            budget: None,
        })
    }

    /// Give each later analysis at most `ms` milliseconds (0: no limit).
    /// The memo keeps what an interrupted one finished, so asking again
    /// goes on from there.
    pub fn set_budget(&mut self, ms: f64) {
        self.budget = (ms > 0.0).then_some(ms);
    }

    /// The `suit_combination::Analysis` of `goal`, or, out of budget or
    /// memory, `{goal, interrupted: true, bound, memory}` with an upper bound
    /// on its score; `memory` says asking again is no use
    fn analyze(&mut self, goal: Goal) -> Result<serde_json::Value, JsValue> {
        let deadline = self.budget.map(|ms| now() + ms);
        self.solver
            .set_interrupt(move |_| memory() > MEMORY_CAP || deadline.is_some_and(|d| now() >= d));
        match self.solver.analyze(goal) {
            Ok(a) => serde_json::to_value(&a).map_err(err),
            Err(Error::Interrupted { bound, .. }) => Ok(serde_json::json!({
                "goal": goal,
                "interrupted": true,
                "bound": bound,
                "memory": memory() > MEMORY_CAP,
            })),
            Err(e) => Err(err(e)),
        }
    }

    /// [`analyze`](Self::analyze) for at least `target` tricks, as JSON
    pub fn target(&mut self, target: u8) -> Result<String, JsValue> {
        Ok(self.analyze(Goal::Tricks { target })?.to_string())
    }

    /// Every target from one trick up to the longer hand's length: a JSON
    /// array of what [`target`](Self::target) returns, each within the budget
    pub fn targets(&mut self) -> Result<String, JsValue> {
        let all = (1..=self.max_tricks)
            .map(|target| self.analyze(Goal::Tricks { target }))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(serde_json::Value::from(all).to_string())
    }

    /// [`analyze`](Self::analyze) for expected tricks as JSON
    pub fn expected(&mut self) -> Result<String, JsValue> {
        Ok(self.analyze(Goal::ExactExpected)?.to_string())
    }

    /// Deal `west` the given cards (East gets the rest) and start playing
    /// for `target` tricks, or for expected tricks when `target` is absent.  Returns the opening snapshot.  Play runs to the
    /// end of each call, whatever the budget, on what the analysis memoized.
    pub fn start(&mut self, west: &str, target: Option<u8>) -> Result<String, JsValue> {
        let west: Holding = west.parse().map_err(|e| err(format!("West: {e}")))?;
        let goal = target.map_or(Goal::ExactExpected, |target| Goal::Tricks { target });
        self.game = Some(Game::new(&mut self.solver, west, goal).map_err(err)?);
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
