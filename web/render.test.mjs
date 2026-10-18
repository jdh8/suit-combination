// `node web/render.test.mjs`: the bridge words for hand-built plans
import assert from 'node:assert/strict';
import { headline, planItems, value } from './render.js';

const made = { kind: 'made' };
const play = (seat, cards, then = made) => ({ kind: 'declarer', seat, cards, then });
const defend = (seat, ...branches) => ({ kind: 'defender', seat, branches });

// AQ opposite 32: low to the Q, the A if West covers
const finesse = play('South', '32', defend('West', [[4, 5], play('North', 'Q')], [[13], play('North', 'A')]));
assert.equal(headline(finesse, 'AQ', '32'), 'South leads the 3/2; if West plays 4/5, North finesses the Q.');
assert.match(planItems(finesse, 'AQ', '32'), /West plays K:.*North wins with the A/);

// AJ9 opposite T32: the 10 led and run, ducked if West covers
const run = play('South', 'T', defend('West', [[4], play('North', '9')], [[13], play('North', '9')]));
assert.equal(headline(run, 'AJ9', 'T32'), 'South leads the 10; if West plays 4, North plays the 9, letting the 10 run.');
assert.match(planItems(run, 'AJ9', 'T32'), /West plays K:.*North ducks, playing the 9/);

// A top card is cashed; one defender branch and showing out say nothing
const cash = play('North', 'A', defend('West', [[2, null], play('South', '3', defend('East', [[4], made]))]));
assert.equal(headline(cash, 'AK', '3'), 'North cashes the A; South plays the 3.');
assert.equal(value(1, 4), '25.0%');
assert.equal(value(0, 4), '0.0%');
assert.equal(value(4, 4), '100.0%');
assert.equal(planItems(made, 'A', '2'), '<li class="made">✓ made</li>');
assert.equal(planItems({ kind: 'hopeless' }, 'A', '2'), '<li class="hopeless">cannot be made from here — play on for what it is worth</li>');
console.log('ok');
