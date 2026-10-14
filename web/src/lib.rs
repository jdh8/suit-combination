//! Wasm binding for the suit-combination web UI: one call per problem,
//! returning every goal's analysis as JSON; JS does the rest.

use contract_bridge::Holding;
use wasm_bindgen::prelude::{JsValue, wasm_bindgen};

/// Analyze `north` opposite `south` (ranks in descending order, e.g. `AJ52`)
/// for every target and for expected tricks.  Returns a JSON array of
/// `suit_combination::Analysis`, or throws a message.
#[wasm_bindgen]
pub fn analyze(north: &str, south: &str) -> Result<String, JsValue> {
    // A JsValue error (rather than a String) keeps wasm-bindgen's externref
    // table in the binary; without any JsValue the CLI fails to find it.
    let err = |e: String| JsValue::from_str(&e);
    let north: Holding = north.parse().map_err(|e| err(format!("North: {e}")))?;
    let south: Holding = south.parse().map_err(|e| err(format!("South: {e}")))?;
    let all = suit_combination::analyze_all(north, south).map_err(|e| err(e.to_string()))?;
    serde_json::to_string(&all).map_err(|e| err(e.to_string()))
}

