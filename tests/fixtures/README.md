# Reference cases

`cases.json` holds one record per case, checked by `tests/fixtures.rs`.
`corpus.txt` is the benchmark corpus for `examples/bench.rs` (one `NORTH
SOUTH` per line, `-` for a void; generated with seed 1, then the known slow
cases appended).

Every case uses the engine's default model unless a future field says
otherwise: omniscient falsecarding defence, 13/13 vacant places, unlimited
entries, no ruffs, no fixed plays, only this suit's tricks count.

| Field | Meaning |
| --- | --- |
| `north`, `south` | Holdings, ranks descending |
| `goal` | Trick target (integer) or `"expected"` (exact expected tricks) |
| `score` | Exact result out of `TOTAL = C(26,13)` (deals for a target; tricks x TOTAL for expected) |
| `value`, `tolerance` | Published rounded result and its half-interval (tolerance defaults to 0.00005) |
| `leads` | Ranks whose opening play must be optimal for the goal |
| `avoid` | Ranks whose opening play must be strictly worse |
| `source` | Where the value or play is published, or the hand derivation |
| `checked` | Date the source was read |
| `note` | Free text |

A competitor's output is never pasted in as `score` or `value`: use a
published document, a hand derivation, or (marked as such) this engine.
