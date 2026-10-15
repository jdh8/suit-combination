// Runs the wasm solver off the main thread so a hard search never freezes the
// page; the main thread terminates this worker to abort one.  One `Trainer`
// at a time: `solve` replaces it, every other op is a Trainer method.
import init, { Trainer } from './pkg/suit_combination_web.js';

let trainer = null;
const ready = init();

onmessage = async ({ data: { id, op, args } }) => {
  await ready;
  try {
    let result;
    if (op === 'solve') {
      trainer?.free();
      trainer = null;
      trainer = new Trainer(...args);
      result = trainer.analyze();
    } else if (trainer) {
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
