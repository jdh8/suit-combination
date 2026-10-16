//! Wasm binding for the suit-combination web UI: one [`Trainer`] per
//! combination holds the memoized solver, answers the analysis of every goal
//! as JSON, and plays the suit out trick by trick.

use contract_bridge::Holding;
use suit_combination::{Game, Goal, Solver};
use wasm_bindgen::prelude::{JsValue, wasm_bindgen};

#[wasm_bindgen]
pub struct Trainer {
    solver: Solver,
    game: Option<Game>,
    max_tricks: u8,
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
        })
    }

    /// Every target from one trick up to the longer hand's length: a JSON
    /// array of `suit_combination::Analysis`.  Separate from [`Self::expected`]
    /// because expected tricks keep full vectors and can take far longer.
    pub fn targets(&mut self) -> Result<String, JsValue> {
        let all = (1..=self.max_tricks)
            .map(|target| self.solver.analyze(Goal::Tricks { target }))
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        serde_json::to_string(&all).map_err(err)
    }

    /// The `suit_combination::Analysis` of expected tricks as JSON
    pub fn expected(&mut self) -> Result<String, JsValue> {
        let a = self.solver.analyze(Goal::Expected).map_err(err)?;
        serde_json::to_string(&a).map_err(err)
    }

    /// Deal `west` the given cards (East gets the rest) and start playing
    /// for `target` tricks, or for expected tricks when `target` is absent.
    /// Returns the opening snapshot.
    pub fn start(&mut self, west: &str, target: Option<u8>) -> Result<String, JsValue> {
        let west: Holding = west.parse().map_err(|e| err(format!("West: {e}")))?;
        let goal = target.map_or(Goal::Expected, |target| Goal::Tricks { target });
        self.game = Some(Game::new(&self.solver, west, goal).map_err(err)?);
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
