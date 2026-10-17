// Runs the wasm solver off the main thread so a hard search never freezes the
// page; the main thread terminates this worker as the hard stop.  One
// `Trainer` at a time: `load` replaces it, every other op is a Trainer
// method, run within `budget` ms when given (an analysis out of time answers
// `{interrupted: true, bound}` and keeps its memo for the next call).
import init, { Trainer } from './pkg/suit_combination_web.js';

let trainer = null;
const ready = init();

onmessage = async ({ data: { id, op, args, budget } }) => {
  await ready;
  try {
    let result = null;
    if (op === 'load') {
      trainer?.free();
      trainer = null;
      trainer = new Trainer(...args);
    } else if (trainer) {
      trainer.set_budget(budget ?? 0);
      result = trainer[op](...args);
    } else {
      throw new Error('no problem');
    }
    postMessage({ id, result });
  } catch (e) {
    // A trap (e.g. out of memory) leaves the wasm heap unusable: say so and
    // let the main thread replace this worker.
    postMessage({ id, error: String(e), fatal: e instanceof WebAssembly.RuntimeError });
  }
};
