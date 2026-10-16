// Thin UI over the wasm solver, which runs in a worker (worker.js): one
// `Trainer` per combination returns every goal's optimal line; JS picks an
// interesting goal, deals the defenders' cards, and plays the suit out,
// grading each card declarer chooses.

const TOTAL = 10400600; // C(26, 13), the denominator of every root score
const RANKS = '23456789TJQKA'; // index + 2 = rank value used in the JSON
const SUITS = [['♠', 's-s'], ['♥', 's-h'], ['♦', 's-d'], ['♣', 's-c']];
const id = (x) => document.getElementById(x);

// { north, south, suit, analysis, all, snap, plays, log, lead, done, busy }
let problem = null;

// --- solver worker ----------------------------------------------------------

// Each call is a message with an id; the reply carries the same id.  A search
// cannot be interrupted, so a timeout or cancel terminates the worker and
// spawns a fresh one, failing every call in flight.
const RANDOM_BUDGET = 2000; // ms a random combination may take before we deal another
const EXACT_BUDGET = 10000; // ms the exact expected-tricks search gets on a combination you enter
let worker;
let nextId = 0;
const pending = new Map(); // id -> { resolve, reject, timer }

function spawn() {
  worker = new Worker('./worker.js', { type: 'module' });
  worker.onmessage = ({ data }) => {
    const p = pending.get(data.id);
    if (!p) return;
    pending.delete(data.id);
    clearTimeout(p.timer);
    if (data.error === undefined) p.resolve(data.result);
    else p.reject(new Error(data.fatal ? `solver crashed, probably out of memory (${data.error})` : data.error));
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

function call(op, args, ms) {
  return new Promise((resolve, reject) => {
    const n = nextId++;
    const timer = ms && setTimeout(() => abort(`gave up after ${ms / 1000} s`), ms);
    pending.set(n, { resolve, reject, timer });
    worker.postMessage({ id: n, op, args });
  });
}

// Solves a combination: every target, then expected tricks if `ev`, each
// within `ms` (no limit when undefined).  Expected tricks first come from the
// targets' lines, which is quick; when the bound does not prove that line
// best, the exact search, which can take minutes, gets `ms` or EXACT_BUDGET.
// Running out of time or cancelling there keeps the line; the worker
// restarts, so the targets and the line are solved again for play.  Only one
// search runs at a time.
async function solve(north, south, ms, ev = true) {
  if (pending.size) abort('superseded');
  id('cancel').classList.remove('hidden');
  try {
    const all = JSON.parse(await call('solve', [north, south], ms));
    if (!ev) return all;
    const line = JSON.parse(await call('expected', [false], ms));
    if (line.score === line.bound) {
      all.push(line);
      return all;
    }
    try {
      all.push(JSON.parse(await call('expected', [true], ms ?? EXACT_BUDGET)));
    } catch (e) {
      if (!e.message.startsWith('gave up') && e.message !== 'cancelled') throw e;
      await call('solve', [north, south]);
      all.push(JSON.parse(await call('expected', [false])));
    }
    return all;
  } finally {
    id('cancel').classList.add('hidden');
  }
}

async function main() {
  spawn();
  id('new').onclick = newProblem;
  id('solve').onclick = customProblem;
  id('cancel').onclick = () => abort('cancelled');
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
  info('Dealing…');
  try {
    for (let attempt = 0; attempt < 200; attempt++) {
      const [north, south] = randomHoldings(lengths[Math.floor(Math.random() * lengths.length)]);
      let all;
      try {
        all = await solve(north, south, RANDOM_BUDGET);
      } catch (e) {
        if (String(e.message).startsWith('gave up')) continue; // too hard to wait for: deal another
        throw e;
      }
      const goals = all.filter((a) => (kind === 'any' || (a.goal.kind === 'tricks') === (kind === 'tricks')) && interesting(a));
      if (goals.length === 0) continue;
      await setProblem(north, south, all, goals[Math.floor(Math.random() * goals.length)]);
      return;
    }
    info('Could not find an interesting problem with these settings.', true);
  } catch (e) {
    if (e.message !== 'superseded') info(String(e.message), true); // a newer request took over
  }
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
  info('Solving… (some combinations take a while)');
  const wantEV = goalText === '' || goalText.startsWith('e');
  let all;
  try {
    all = await solve(north, south, undefined, wantEV);
  } catch (e) {
    if (e.message !== 'superseded') info(String(e.message), e.message !== 'cancelled');
    return;
  }
  let analysis;
  if (wantEV) analysis = all.find((a) => a.goal.kind !== 'tricks');
  else analysis = all.find((a) => a.goal.kind === 'tricks' && a.goal.target === Number(goalText));
  if (!analysis) {
    info(`No such goal: ${goalText}`, true);
    return;
  }
  await setProblem(north, south, all, analysis);
}

async function setProblem(north, south, all, analysis) {
  // West's cards in the suit are those among a random 13 of the 26 unseen
  // cards, so every split comes up with its vacant-places odds.
  const missing = [...RANKS].filter((c) => !north.includes(c) && !south.includes(c)).map((c) => RANKS.indexOf(c) + 2);
  const west = fmt(shuffle([...missing, ...Array(26 - missing.length).fill(0)]).slice(0, 13).filter(Boolean));
  const target = analysis.goal.kind === 'tricks' ? analysis.goal.target : undefined;
  const snap = JSON.parse(await call('start', [west, target, analysis.goal.kind === 'exact_expected']));
  const suit = SUITS[Math.floor(Math.random() * SUITS.length)];
  problem = { north, south, suit, analysis, all, snap, plays: [...snap.plays], log: [], lead: null, done: false, busy: false };
  info('');
  render();
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
    + (playing ? `<span>Best from here: ${value(analysis.goal, Math.max(...snap.moves.map((m) => m.score)), snap.weight)}</span>
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
      + (ok ? '' : `; best ${value(goal, best, snap.weight)} with ${alternatives}`),
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
  const { analysis, all, snap, lead, log } = problem;
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
  const played = log.length ? ` — ${mistakes === 0 ? 'every card was best' : `${mistakes} card${mistakes === 1 ? '' : 's'} not best`}` : '';
  const rows = [...analysis.leads]
    .sort((a, b) => b.score - a.score)
    .map((l) => `<tr class="${l.score === best ? 'best' : ''} ${lead && l.seat === lead.seat && l.cards === lead.cards ? 'chosen' : ''}">
      <td>${l.seat}: ${cardsText(l.cards)}</td><td class="num">${value(goal, l.score, TOTAL)}</td></tr>`)
    .join('');
  const goals = all.map((a) => `<span class="${a === analysis ? 'active' : ''}">${a.goal.kind === 'tricks' ? `≥${a.goal.target}: ` : 'EV: '}${value(a.goal, a.score, TOTAL)}${a.bound > a.score ? ` (best ≤ ${(a.bound / TOTAL).toFixed(2)})` : ''}</span>`).join('');
  // The expected-tricks line when neither its bound nor the exact search
  // (out of time) showed it is the best
  const unproven = analysis.bound > analysis.score
    ? `<p class="note">Best line found; the best possible is at most ${value(goal, analysis.bound, TOTAL)}, so a line here may be graded best that is not.</p>`
    : '';
  const fb = id('feedback');
  fb.classList.remove('hidden');
  fb.innerHTML = `
    <p class="verdict ${ok ? 'ok' : 'no'}">${verdict}${played}</p>
    <div class="panel-title">Every lead</div>
    <table class="leads">${rows}</table>
    <div class="panel-title">Optimal line</div>${unproven}
    <div class="plan"><ul>${planItems(analysis.plan, goal)}</ul></div>
    <div class="panel-title">This combination</div>
    <div class="goals">${goals}</div>
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
