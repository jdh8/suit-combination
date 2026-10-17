// Thin UI over the wasm solver, which runs in a worker (worker.js): one
// `Trainer` per combination answers each goal's analysis.  JS solves the
// goals in short slices so each result shows as it arrives, picks an
// interesting goal, deals the defenders' cards, and plays the suit out,
// grading each card declarer chooses.

const TOTAL = 10400600; // C(26, 13), the denominator of every root score
const RANKS = '23456789TJQKA'; // index + 2 = rank value used in the JSON
const SUITS = [['♠', 's-s'], ['♥', 's-h'], ['♦', 's-d'], ['♣', 's-c']];
const id = (x) => document.getElementById(x);

// { search, north, south, suit, analysis, snap, plays, log, lead, done, busy }
let problem = null;
// The combination being solved (see newSearch)
let search = null;

// --- solver worker ----------------------------------------------------------

// Each call is a message with an id; the reply carries the same id.  An
// analysis given a budget stops itself in time (give or take one costly
// state) and keeps what it finished, so the next call goes on from there.
// Terminating the worker is the hard stop: it fails every call in flight and
// loses the memo.
const SLICE = 300; // ms per analysis call, so results and bounds show as they come
const SESSION = 10000; // ms a search on a combination you enter runs before it waits for "keep searching"
const RANDOM_BUDGET = 2000; // ms a random combination may take before we deal another
const HARD_STOP = 100; // ms a cancelled call may take to return before the worker is terminated
let worker;
let loaded = null; // `${north} ${south}` of the Trainer in the worker
let full = false; // an analysis stopped at the worker's memory cap, and wasm memory never shrinks
let nextId = 0;
const pending = new Map(); // id -> { resolve, reject, timer }

function spawn() {
  worker = new Worker('./worker.js', { type: 'module' });
  loaded = null;
  full = false;
  worker.onmessage = ({ data }) => {
    const p = pending.get(data.id);
    if (!p) return;
    pending.delete(data.id);
    clearTimeout(p.timer);
    if (data.error === undefined) p.resolve(data.result);
    else p.reject(Object.assign(new Error(data.fatal ? `solver crashed, probably out of memory (${data.error})` : data.error), { fatal: data.fatal }));
    if (data.fatal) abort(data.error);
  };
  worker.onerror = (e) => abort(e.message || 'solver crashed');
}

function abort(reason) {
  worker.terminate();
  for (const p of pending.values()) {
    clearTimeout(p.timer);
    p.reject(new Error(reason));
  }
  pending.clear();
  spawn();
}

// `budget`: ms the analysis may take; `ms`: terminate the worker if no
// answer comes in that time
function call(op, args = [], budget = undefined, ms = undefined) {
  return new Promise((resolve, reject) => {
    const n = nextId++;
    const timer = ms && setTimeout(() => abort(`gave up after ${(ms / 1000).toFixed(1)} s`), ms);
    pending.set(n, { resolve, reject, timer });
    worker.postMessage({ id: n, op, args, budget });
  });
}

// --- goals and searches -----------------------------------------------------

// Goals by key: `t3` for three tricks, `ev` for the targets' combined line,
// `exact` for the exact expected-tricks search
const goalKey = (goal) => (goal.kind === 'tricks' ? `t${goal.target}` : goal.kind === 'expected' ? 'ev' : 'exact');
const goalOf = (key) => (key === 'ev' ? { kind: 'expected' } : key === 'exact' ? { kind: 'exact_expected' } : { kind: 'tricks', target: Number(key.slice(1)) });
const goalLabel = (key) => (key === 'ev' ? 'EV' : key === 'exact' ? 'EV (exact)' : `≥${key.slice(1)}`);
// An Analysis, not {interrupted, bound}
const finished = (r) => r !== undefined && !r.interrupted;
// Searched as far as it can go: finished, or stopped for memory
const closed = (r) => finished(r) || r?.memory;
const secs = (ms) => `${(ms / 1000).toFixed(1)} s`;

function solveGoal(key, budget, ms) {
  const [op, args] = key === 'ev' ? ['expected', [false]] : key === 'exact' ? ['expected', [true]] : ['target', [Number(key.slice(1))]];
  return call(op, args, budget, ms).then(JSON.parse);
}

// A combination's goals in the order to solve them, the one asked for
// first, with each one's latest result.  `warm`: goals whose analysis the
// worker's memo holds, which play needs.
function newSearch(north, south, want, visible) {
  if (search?.running) {
    search.stop = true;
    if (pending.size) abort('superseded');
  }
  const targets = Array.from({ length: Math.max(north.length, south.length) }, (_, i) => `t${i + 1}`);
  const keys = [...new Set([want, ...targets, 'ev'])];
  search = { north, south, want, keys, results: new Map(), warm: new Set(), running: false, stop: false, current: null, since: 0, visible };
  problem = null;
  id('table').innerHTML = '';
  id('feedback').classList.add('hidden');
  showSearch();
  return search;
}

async function load(s) {
  const key = `${s.north} ${s.south}`;
  if (loaded === key) return;
  if (full && !pending.size) {
    worker.terminate();
    spawn();
  }
  await call('load', [s.north, s.south]);
  loaded = key;
  s.warm.clear();
}

// The goal asked for is done: for expected tricks, the line is proven best
// or the exact search has finished too.
function settled(s) {
  const r = s.results.get(s.want);
  if (!finished(r)) return false;
  return s.want !== 'ev' || r.proven || finished(s.results.get('exact'));
}

// Expected tricks not asked for wait for the targets, whose searches they
// would only go on with (where one costly state can hold a slice for seconds)
const waits = (s, key) => key !== s.want && !key.startsWith('t') && s.keys.some((k) => k.startsWith('t') && !closed(s.results.get(k)) && s.results.has(k));

// Solves the open goals of `s` in slices of SLICE ms, round robin, until
// all are finished, the goal asked for is settled (after every goal has had
// a turn), SESSION ms pass, or the user cancels; then, if no game is on yet,
// plays the goal asked for when it is finished.  Cancelling keeps every
// result so far.
async function runSearch(s) {
  s.running = true;
  s.stop = false;
  s.since = performance.now();
  const ticker = setInterval(showSearch, 100);
  showSearch();
  let failed = null;
  try {
    await load(s);
    while (!s.stop && performance.now() - s.since < SESSION) {
      const open = s.keys.filter((k) => !closed(s.results.get(k)));
      if (!open.length) break;
      for (const key of open) {
        if (s.stop || performance.now() - s.since >= SESSION) break;
        if (waits(s, key)) continue;
        s.current = key;
        showSearch();
        const r = await solveGoal(key, SLICE);
        if (search !== s) throw new Error('superseded');
        s.results.set(key, r);
        if (finished(r)) s.warm.add(key);
        full ||= r.memory;
        // The combined line is not proven best: the exact search may beat it.
        if (key === 'ev' && s.want === 'ev' && finished(r) && !r.proven && !s.keys.includes('exact')) s.keys.push('exact');
      }
      if (settled(s)) break;
    }
  } catch (e) {
    failed = e.message;
    // A crash (out of memory) lost the memo: keep the goal's last bound, and
    // do not try it again.
    if (e.fatal && s.current) s.results.set(s.current, { ...s.results.get(s.current), interrupted: true, memory: true });
  }
  s.current = null;
  showSearch();
  try {
    // Play what has finished, after a cancel or a crash too
    if (search === s && problem?.search !== s) {
      const exact = s.results.get('exact');
      const a = s.want === 'ev' && finished(exact) ? exact : s.results.get(s.want);
      if (finished(a)) await setProblem(s, a);
    }
  } catch (e) {
    failed = e.message;
  }
  clearInterval(ticker);
  if (search !== s) return;
  s.running = false;
  if (!s.results.size) s.visible = false;
  showSearch();
  if (failed && failed !== 'cancelled') info(failed, true);
  else if (!finished(s.results.get(s.want))) info(`${goalLabel(s.want)} not solved yet${closed(s.results.get(s.want)) ? '' : ': keep searching, or try another combination'}`);
}

// Stops the search: the call in flight ends at its budget, and past
// HARD_STOP the worker is terminated, unless it holds the game being played.
function cancel() {
  const s = search;
  if (!s?.running) return;
  s.stop = true;
  setTimeout(() => {
    if (search === s && s.running && problem?.search !== s) abort('cancelled');
  }, HARD_STOP);
}

// One goal's status: proven, best found (with the bound), or interrupted
// (with the bound); `solving` while its slice runs
function goalStatus(s, key) {
  const r = s.results.get(key);
  const label = `<b>${goalLabel(key)}</b>`;
  const at = (goal, score) => value(goal, score, TOTAL);
  // An interrupted search's bound, if any
  const bound = r?.interrupted && r.bound !== undefined ? ` (≤ ${at(goalOf(key), r.bound)})` : '';
  if (s.current === key && s.running) return `${label} solving…${bound}`;
  if (waits(s, key) && !r) return `${label} waiting for the targets`;
  if (r === undefined) return `${label} waiting`;
  if (r.memory) return `${label} out of memory${bound}`;
  if (r.interrupted) return `${label} interrupted${bound}`;
  if (r.proven) return `${label} ${at(r.goal, r.score)} proven`;
  return `${label} ${at(r.goal, r.score)} best found (≤ ${at(r.goal, r.bound)})`;
}

// The goals strip, the buttons, and while searching the elapsed time
function showSearch() {
  const s = search;
  const el = id('results');
  el.classList.toggle('hidden', !s?.visible);
  if (s?.visible) {
    el.innerHTML = s.keys
      .map((k) => `<span class="${k === s.want ? 'active' : ''}">${goalStatus(s, k)}</span>`)
      .join('');
  }
  id('cancel').classList.toggle('hidden', !s?.running);
  const open = s?.visible && s.keys.some((k) => !closed(s.results.get(k)));
  id('more').classList.toggle('hidden', !open || s.running);
  if (s?.running && s.visible && (s.current || problem?.search !== s)) {
    const doing = s.current ? `Solving ${goalLabel(s.current)}` : 'Preparing play';
    info(`${doing}… ${secs(performance.now() - s.since)}`);
  }
}

async function main() {
  spawn();
  id('new').onclick = newProblem;
  id('solve').onclick = customProblem;
  id('cancel').onclick = cancel;
  id('more').onclick = () => search && !search.running && runSearch(search);
  for (const el of ['c-north', 'c-south', 'c-goal']) {
    id(el).onkeydown = (ev) => { if (ev.key === 'Enter') customProblem(); };
  }
  newProblem();
}

// --- problem generation ----------------------------------------------------

function shuffle(a) {
  for (let i = a.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [a[i], a[j]] = [a[j], a[i]];
  }
  return a;
}

const fmt = (ranks) => ranks.sort((a, b) => b - a).map((r) => RANKS[r - 2]).join('');

function randomHoldings(total) {
  const ranks = shuffle([...RANKS].map((_, i) => i + 2)).slice(0, total);
  const k = 1 + Math.floor(Math.random() * (total - 1)); // North's length
  return [fmt(ranks.slice(0, k)), fmt(ranks.slice(k))];
}

// A goal is worth asking about when the leads differ enough to matter and the
// answer is neither nearly certain nor nearly hopeless.
function interesting(a) {
  const scores = a.leads.map((l) => l.score);
  const best = Math.max(...scores) / TOTAL;
  const worst = Math.min(...scores) / TOTAL;
  if (a.goal.kind === 'tricks') return best >= 0.2 && best <= 0.95 && best - worst >= 0.03;
  return best - worst >= 0.05; // expected tricks at stake
}

async function newProblem() {
  const lengths = id('length').value.split(',').map(Number);
  const kind = id('goalkind').value;
  const started = performance.now();
  const ticker = setInterval(() => info(`Dealing… ${secs(performance.now() - started)}`), 100);
  info('Dealing…');
  try {
    for (let attempt = 0; attempt < 200; attempt++) {
      const [north, south] = randomHoldings(lengths[Math.floor(Math.random() * lengths.length)]);
      const s = newSearch(north, south, 't1', false);
      const all = await solveRandom(s);
      if (search !== s) return; // a newer request took over
      if (!all) continue; // too hard to wait for: deal another
      const goals = all.filter((a) => (kind === 'any' || (a.goal.kind === 'tricks') === (kind === 'tricks')) && interesting(a));
      if (goals.length === 0) continue;
      const pick = goals[Math.floor(Math.random() * goals.length)];
      s.want = goalKey(pick.goal);
      clearInterval(ticker);
      await setProblem(s, pick);
      return;
    }
    info('Could not find an interesting problem with these settings.', true);
  } catch (e) {
    if (e.message !== 'superseded') info(String(e.message), e.message !== 'cancelled');
  } finally {
    clearInterval(ticker);
  }
}

// Every target and expected tricks of a random combination within
// RANDOM_BUDGET in all, or null if that is not enough.  When the line is
// not proven best, the exact search gets the time left, and replaces the
// line if it finishes.
async function solveRandom(s) {
  const deadline = performance.now() + RANDOM_BUDGET;
  const left = () => deadline - performance.now();
  s.running = true;
  try {
    await load(s);
    for (const key of s.keys) {
      if (left() <= 0) return null;
      const r = await solveGoal(key, left(), left() + 1000);
      if (r.interrupted) return null;
      s.results.set(key, r);
      s.warm.add(key);
    }
    if (!s.results.get('ev').proven && left() > 0) {
      s.keys.push('exact');
      const r = await solveGoal('exact', left(), left() + 1000);
      s.results.set('exact', r);
      if (finished(r)) s.warm.add('exact');
    }
  } catch (e) {
    if (String(e.message).startsWith('gave up')) return null;
    throw e;
  } finally {
    s.running = false;
  }
  const exact = s.results.get('exact');
  return s.keys
    .filter((k) => k.startsWith('t'))
    .map((k) => s.results.get(k))
    .concat(finished(exact) ? exact : s.results.get('ev'));
}

// Accepts "aj52", "A J 10 2", "AJT2"; returns ranks in descending order.
function normalize(text) {
  const seen = new Set();
  for (const tok of text.toUpperCase().replace(/10/g, 'T').match(/[2-9TJQKA]/g) || []) seen.add(tok);
  return fmt([...seen].map((c) => RANKS.indexOf(c) + 2));
}

async function customProblem() {
  const north = normalize(id('c-north').value);
  const south = normalize(id('c-south').value);
  const goalText = id('c-goal').value.trim().toLowerCase();
  const wantEV = goalText === '' || goalText.startsWith('e');
  const target = Number(goalText);
  if (!wantEV && !(Number.isInteger(target) && target >= 1 && target <= Math.max(north.length, south.length))) {
    info(`No such goal: ${goalText}`, true);
    return;
  }
  await runSearch(newSearch(north, south, wantEV ? 'ev' : `t${target}`, true));
}

// Deals the defenders' cards and starts playing for `analysis`
async function setProblem(s, analysis) {
  const key = goalKey(analysis.goal);
  await load(s);
  // A cancel may have replaced the worker since: solve the goal again for play.
  if (!s.warm.has(key)) {
    await solveGoal(key);
    s.warm.add(key);
  }
  // West's cards in the suit are those among a random 13 of the 26 unseen
  // cards, so every split comes up with its vacant-places odds.
  const { north, south } = s;
  const missing = [...RANKS].filter((c) => !north.includes(c) && !south.includes(c)).map((c) => RANKS.indexOf(c) + 2);
  const west = fmt(shuffle([...missing, ...Array(26 - missing.length).fill(0)]).slice(0, 13).filter(Boolean));
  const target = analysis.goal.kind === 'tricks' ? analysis.goal.target : undefined;
  const snap = JSON.parse(await call('start', [west, target, analysis.goal.kind === 'exact_expected']));
  if (search !== s) return;
  const suit = SUITS[Math.floor(Math.random() * SUITS.length)];
  problem = { search: s, north, south, suit, analysis, snap, plays: [...snap.plays], log: [], lead: null, done: false, busy: false };
  s.visible = true;
  info('');
  render();
  showSearch();
  id('feedback').classList.add('hidden');
}

function info(text, error = false) {
  id('info').textContent = text;
  id('info').classList.toggle('error', error);
}

// --- rendering --------------------------------------------------------------

function goalText(goal) {
  return goal.kind === 'tricks'
    ? `How do you play for ${goal.target} trick${goal.target === 1 ? '' : 's'}?`
    : 'How do you play for the most tricks on average?';
}

// `score` out of `weight`: a probability or an expected number of tricks
function value(goal, score, weight) {
  return goal.kind === 'tricks' ? `${(score / weight * 100).toFixed(1)}%` : `${(score / weight).toFixed(2)} tricks`;
}

const cardText = (c) => (c === 'T' ? '10' : c);
const cardsText = (cards) => [...cards].map(cardText).join('/');
const rankText = (r) => (r === null ? '–' : cardText(RANKS[r - 2]));

function handRow(seat, inner) {
  const row = document.createElement('div');
  row.className = 'hand';
  row.innerHTML = `<span class="label">${seat}</span><span class="suit ${problem.suit[1]}">${problem.suit[0]}</span>`;
  row.appendChild(inner);
  return row;
}

// Declarer's hand as buttons, live when that seat may play
function declarerRow(seat, cards, live) {
  const box = document.createElement('span');
  box.className = 'cards';
  for (const rank of cards) {
    const b = document.createElement('button');
    b.className = 'card';
    b.textContent = cardText(rank);
    b.disabled = !live;
    b.onclick = () => play(rank);
    box.appendChild(b);
  }
  return handRow(seat, box);
}

// A defender's cards played so far; the one on the table now is marked
function defenderRow(seat, trick) {
  const box = document.createElement('span');
  box.className = 'played';
  const now = trick.find((p) => p.seat === seat);
  box.innerHTML = problem.plays
    .filter((p) => p.seat === seat && p.card !== null)
    .map((p) => `<span class="${p === now ? 'now' : ''}">${rankText(p.card)}</span>`)
    .join(' ') || (now ? '<span class="now">–</span>' : '');
  return handRow(seat, box);
}

function render() {
  const { analysis, snap, plays, done, log } = problem;
  const el = id('table');
  el.innerHTML = '';
  const playing = !done && !snap.over;
  const turn = new Set(playing ? snap.moves.map((m) => m.seat) : []);
  // The trick on the table: the last partial one, or the last complete one
  // once the suit is played out.
  const n = plays.length % 4 || (snap.over ? 4 : 0);
  const trick = plays.slice(plays.length - n);

  el.appendChild(declarerRow('North', snap.north, turn.has('North')));
  const ew = document.createElement('div');
  ew.className = 'ew';
  ew.appendChild(defenderRow('West', trick));
  ew.appendChild(defenderRow('East', trick));
  el.appendChild(ew);
  el.appendChild(declarerRow('South', snap.south, turn.has('South')));

  const q = document.createElement('p');
  q.className = 'question';
  q.textContent = goalText(analysis.goal);
  el.appendChild(q);

  const t = document.createElement('p');
  t.className = 'trick';
  const number = Math.floor((plays.length - 1) / 4) + 1;
  t.innerHTML = trick.length
    ? `Trick ${number}: ${trick.map((p) => `${p.seat[0]} ${rankText(p.card)}`).join(' · ')}`
    : 'Lead from either hand.';
  if (playing && trick.length) t.innerHTML += ` — ${[...turn].join(' or ')} to play`;
  el.appendChild(t);

  const s = document.createElement('p');
  s.className = 'status';
  s.innerHTML = `<span>Tricks won: ${snap.won}</span>`
    + (playing ? `<span>${analysis.proven ? 'Best' : 'Best found'} from here: ${value(analysis.goal, Math.max(...snap.moves.map((m) => m.score)), snap.weight)}</span>
      <button class="secondary" id="reveal">Show solution</button>` : '');
  el.appendChild(s);
  if (playing) id('reveal').onclick = finish;

  if (log.length) {
    const ul = document.createElement('ul');
    ul.className = 'log';
    ul.innerHTML = log.map((l) => `<li class="${l.ok ? 'ok' : 'no'}">${l.text}</li>`).join('');
    el.appendChild(ul);
  }
}

// --- play -------------------------------------------------------------------

async function play(rank) {
  if (problem.busy) return;
  problem.busy = true;
  const { snap, analysis, plays, log } = problem;
  const goal = analysis.goal;
  const chosen = snap.moves.find((m) => m.cards.includes(rank));
  const best = Math.max(...snap.moves.map((m) => m.score));
  const ok = chosen.score === best;
  problem.lead ??= chosen;
  const alternatives = snap.moves
    .filter((m) => m.score === best)
    .map((m) => `${m.seat} ${cardsText(m.cards)}`)
    .join(' or ');
  log.push({
    ok,
    text: `Trick ${Math.floor(plays.length / 4) + 1}, ${chosen.seat} ${cardText(rank)}: ${value(goal, chosen.score, snap.weight)}`
      + (ok ? '' : `; ${analysis.proven ? 'best' : 'best found'} ${value(goal, best, snap.weight)} with ${alternatives}`),
  });
  // Every state was memoized by the analysis, so this is quick.
  let next;
  try {
    next = JSON.parse(await call('play', [rank]));
  } catch (e) {
    info(String(e.message), true);
    return;
  } finally {
    problem.busy = false;
  }
  plays.push(...next.plays);
  problem.snap = next;
  render();
  if (next.over) finish();
}

// Ends the play (if still going) and shows the full analysis.
function finish() {
  const { analysis, snap, lead, log } = problem;
  const goal = analysis.goal;
  problem.done = true;
  render();
  const best = Math.max(...analysis.leads.map((l) => l.score));
  let verdict;
  let ok;
  if (!snap.over) {
    ok = false;
    verdict = 'Solution';
  } else if (goal.kind === 'tricks') {
    ok = snap.won >= goal.target;
    verdict = ok ? `✓ Made: ${snap.won} tricks` : `✗ Down: ${snap.won} of ${goal.target} tricks`;
  } else {
    ok = log.every((l) => l.ok);
    verdict = `${snap.won} tricks taken`;
  }
  const mistakes = log.filter((l) => !l.ok).length;
  const bestWord = analysis.proven ? 'best' : 'the best found';
  const played = log.length ? ` — ${mistakes === 0 ? `every card was ${bestWord}` : `${mistakes} card${mistakes === 1 ? '' : 's'} not ${bestWord}`}` : '';
  // Unproven, each lead shows what no line after it can beat.
  const rows = [...analysis.leads]
    .sort((a, b) => b.score - a.score)
    .map((l) => `<tr class="${l.score === best ? 'best' : ''} ${lead && l.seat === lead.seat && l.cards === lead.cards ? 'chosen' : ''}">
      <td>${l.seat}: ${cardsText(l.cards)}</td><td class="num">${value(goal, l.score, TOTAL)}</td>
      <td class="num">${l.bound > l.score ? `≤ ${value(goal, l.bound, TOTAL)}` : ''}</td></tr>`)
    .join('');
  // The expected-tricks line when neither its bound nor the exact search
  // (out of time) showed it is the best
  const exact = problem.search.results.get('exact');
  const since = finished(exact) && analysis !== exact ? ` The exact search, finished since, gives ${value(goal, exact.score, TOTAL)}.` : '';
  const unproven = analysis.proven
    ? ''
    : `<p class="note">Best line found; the best possible is at most ${value(goal, analysis.bound, TOTAL)}, so a line here may be graded best that is not.${since}</p>`;
  const fb = id('feedback');
  fb.classList.remove('hidden');
  fb.innerHTML = `
    <p class="verdict ${ok ? 'ok' : 'no'}">${verdict}${played}</p>
    <div class="panel-title">Every lead</div>
    <table class="leads">${rows}</table>
    <div class="panel-title">${analysis.proven ? 'Optimal line' : 'Line found'}</div>${unproven}
    <div class="plan"><ul>${planItems(analysis.plan, goal)}</ul></div>
    <p style="margin:0.8rem 0 0"><button class="primary" id="next">Next problem</button></p>`;
  id('next').onclick = newProblem;
}

// Flattens declarer's forced sequence into sibling items; a defender's choice
// opens one nested list per distinct continuation.
function planItems(plan, goal) {
  switch (plan.kind) {
    case 'made':
      return `<li class="made">${goal.kind === 'tricks' ? '✓ made' : '✓ every trick played for is in'}</li>`;
    case 'hopeless':
      return `<li class="hopeless">${goal.kind === 'tricks' ? 'cannot be made from here — play on for what it is worth' : 'no more tricks to be had'}</li>`;
    case 'declarer': {
      const what = plan.cards ? `play the ${cardsText(plan.cards)}` : 'discard';
      return `<li>${plan.seat}: ${what}</li>` + planItems(plan.then, goal);
    }
    case 'defender':
      if (plan.branches.length === 1) return planItems(plan.branches[0][1], goal);
      return plan.branches
        .map(([labels, sub]) => {
          const cards = labels.map((l) => (l === null ? 'nothing' : rankText(l))).join('/');
          return `<li><span class="branch">${plan.seat} plays ${cards}:</span><ul>${planItems(sub, goal)}</ul></li>`;
        })
        .join('');
    default:
      return '';
  }
}

main();
