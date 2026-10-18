// The analysis page: one combination under constraints, every trick target
// and the lines compared at matchpoints or IMPs,
// each line as a tree, in words, and as a play explorer where any defender
// reply can be followed.  The problem lives in the URL hash as JSON, so a
// link reproduces it.

import { RANKS, rankOf, cardText, cardsText, rankText, value, planItems, headline, describe, advance, pick, missing, normalize } from './render.js';

const id = (x) => document.getElementById(x);
const SLICE = 300; // ms per call, so each result shows as it comes
const SESSION = 10000;
const CHOICES = 3000; // ms to weigh the cards at one position // ms before the search waits for "keep searching"
const HARD_STOP = 100; // ms a cancelled call may take before the worker is terminated, losing its memo
const secs = (ms) => `${(ms / 1000).toFixed(1)} s`;
const pct = (w, total) => `${(w / total * 100).toFixed(2)}%`;

// --- solver worker: the protocol of worker.js -------------------------------

let worker;
let loaded = null; // the job id of the Trainer in the worker
let nextId = 0;
const pending = new Map();

function spawn() {
  worker = new Worker('./worker.js', { type: 'module' });
  loaded = null;
  worker.onmessage = ({ data }) => {
    const p = pending.get(data.id);
    if (!p) return;
    pending.delete(data.id);
    if (data.error === undefined) p.resolve(data.result);
    else p.reject(new Error(data.fatal ? `solver crashed, probably out of memory (${data.error})` : data.error));
    if (data.fatal) restart(data.error);
  };
  worker.onerror = (e) => restart(e.message || 'solver crashed');
}

// A trap leaves the wasm heap unusable: start a fresh worker
function restart(reason) {
  worker.terminate();
  for (const p of pending.values()) p.reject(new Error(reason));
  pending.clear();
  spawn();
}

const call = (op, args = [], budget = undefined) => new Promise((resolve, reject) => {
  const n = nextId++;
  pending.set(n, { resolve, reject });
  worker.postMessage({ id: n, op, args, budget });
});

// --- the problem ------------------------------------------------------------

// { id, n, s, c (Constraints JSON), max, keys, results: key -> latest answer,
//   layouts, running, stop, current, since }.  Keys: t3 for three tricks,
//   cmp for the comparison.
let job = null;
// What is shown: tab, target, scoring, line `a` (and `b` to compare), and
// the explorer's defender cards so far
const view = { tab: 'targets', goal: 't1', scoring: { kind: 'matchpoints' }, a: 0, b: null, path: [], at: null };

const finished = (r) => r !== undefined && !r.interrupted && !r.error;
// A comparison still checking its best line is shown but not closed
const closed = (r) => (finished(r) && !r.open) || r?.memory || r?.error !== undefined;
const goalOf = (key) => Number(key.slice(1));

function solve(j, key) {
  const [op, args] = key === 'cmp' ? ['compare', [JSON.stringify(view.scoring)]]
    : ['target', [goalOf(key)]];
  return call(op, args, SLICE).then(JSON.parse);
}

function start({ n, s, c = {}, goal = 't1', tab, scoring }) {
  if (job) job.stop = true;
  // A call in flight may hold the worker for minutes
  if (pending.size) restart('superseded');
  [n, s] = [normalize(n), normalize(s)];
  const live = (h) => [...h].filter((x) => !(c.discards ?? '').includes(x)).length;
  const max = Math.min(Math.max(live(n), live(s)), c.rounds ?? 13);
  const keys = [...Array.from({ length: max }, (_, i) => `t${i + 1}`), 'cmp'];
  job = { id: JSON.stringify([n, s, c]), n, s, c, max, keys, results: new Map(), layouts: null, running: false, stop: false, current: null, since: 0 };
  Object.assign(view, {
    goal: keys.includes(goal) && goal.startsWith('t') ? goal : 't1',
    tab: tab === 'ev' || goal === 'ev' ? 'lines' : tab ?? 'targets',
    scoring: scoring ?? { kind: 'matchpoints' },
    a: 0,
    b: null,
    path: [],
    at: null,
  });
  run(job);
}

// Solves the open goals of `j` a slice at a time, round robin, targets
// first, until all are done, SESSION ms pass, or the user cancels
async function run(j) {
  j.running = true;
  j.stop = false;
  j.since = performance.now();
  const ticker = setInterval(status, 100);
  try {
    if (loaded !== j.id) {
      loaded = null;
      await call('load', [j.n, j.s, JSON.stringify(j.c)]);
      loaded = j.id;
      j.layouts ??= JSON.parse(await call('layouts'));
      show();
    }
    while (!j.stop && job === j && performance.now() - j.since < SESSION) {
      const open = j.keys.filter((k) => !closed(j.results.get(k)));
      if (!open.length) break;
      for (const key of open) {
        if (j.stop || job !== j || performance.now() - j.since >= SESSION) break;
        // The targets show sooner; the comparison's full search is the slow part.
        if (key === 'cmp' && j.keys.some((k) => k.startsWith('t') && !closed(j.results.get(k)))) continue;
        j.current = key;
        let r;
        try {
          r = await solve(j, key);
        } catch (e) {
          if (loaded !== j.id) throw e; // the worker crashed
          r = { error: e.message };
        }
        if (job !== j) return;
        j.results.set(key, r);
        show();
      }
    }
    if (job === j) info(j.keys.every((k) => finished(j.results.get(k)) && !j.results.get(k).open) ? `Solved in ${secs(performance.now() - j.since)}` : '');
  } catch (e) {
    if (job === j) info(e.message, e.message !== 'cancelled');
  } finally {
    clearInterval(ticker);
    j.running = false;
    j.current = null;
    if (job === j) {
      status();
      show();
    }
  }
}

function info(text, error = false) {
  id('info').textContent = text;
  id('info').classList.toggle('error', error);
}

const label = (key) => (key === 'cmp' ? 'Lines' : `≥${key.slice(1)}`);

// The search's progress and its buttons
function status() {
  const j = job;
  id('cancel').classList.toggle('hidden', !j?.running);
  id('more').classList.toggle('hidden', !j || j.running || !j.layouts || j.keys.every((k) => closed(j.results.get(k))));
  if (j?.running) info(`${j.current ? `Solving ${label(j.current)}` : 'Loading'}… ${secs(performance.now() - j.since)}`);
}

// One goal's value, or why there is none yet
function goalStatus(j, key) {
  const r = j.results.get(key);
  const bound = key !== 'cmp' && r?.interrupted && r.bound !== undefined ? ` (≤ ${value(r.bound, total(j))})` : '';
  if (r?.error) return `${label(key)}: ${r.error}`;
  if (finished(r)) return key === 'cmp' ? `Best ${r.best.length > 1 ? 'mix' : 'line'}${r.proven ? '' : r.open ? ' so far' : ' found'}` : `${label(key)} ${value(r.score, r.total)}`;
  if (j.current === key && j.running) return `${label(key)} solving…${bound}`;
  if (r?.memory) return `${label(key)} out of memory${bound}`;
  if (r?.interrupted) return `${label(key)} interrupted${bound}`;
  return `${label(key)} waiting`;
}

const total = (j) => j.layouts.reduce((t, l) => t + l.weight, 0);

// --- constraints form <-> suit_combination::Constraints JSON -----------------

const RANGE = { length: [0, 13], hcp: [0, 10] };

function range(text, [lo, hi]) {
  const t = text.trim();
  if (!t || t === 'any' || t === '-') return [lo, hi];
  const m = t.match(/^(\d*)\s*-\s*(\d*)$/);
  if (m) return [m[1] ? Number(m[1]) : lo, m[2] ? Number(m[2]) : hi];
  if (/^\d+$/.test(t)) return [Number(t), Number(t)];
  throw new Error(`Not a range: ${t}`);
}

const rangeText = ([a, b], [lo, hi]) => (a === lo && b === hi ? '' : a === b ? `${a}` : `${a === lo ? '' : a}-${b === hi ? '' : b}`);

function num(field) {
  const t = id(field).value.trim();
  if (!t) return null;
  if (!/^\d+$/.test(t)) throw new Error(`Not a number: ${t}`);
  return Number(t);
}

function readConstraints() {
  const c = {};
  const vacant = [num('vacant-e'), num('vacant-w')];
  if (vacant.some((x) => x !== null)) c.vacant = vacant.map((x) => x ?? 13);
  const known = [normalize(id('known-e').value), normalize(id('known-w').value)];
  if (known.some(Boolean)) c.known = known;
  for (const k of ['length', 'hcp']) {
    const r = [range(id(`${k}-e`).value, RANGE[k]), range(id(`${k}-w`).value, RANGE[k])];
    if (r.some((x) => rangeText(x, RANGE[k]))) c[k] = r;
  }
  if (id('first').value) c.first = id('first').value;
  const entries = [num('entries-n'), num('entries-s')];
  if (entries.some((x) => x !== null)) c.entries = entries;
  if (num('rounds') !== null) c.rounds = num('rounds');
  const lead = normalize(id('lead').value);
  if (lead.length > 1) throw new Error(`First card: one card, not ${lead}`);
  if (lead) c.lead = rankOf(lead);
  const discards = normalize(id('discards').value);
  if (discards) c.discards = discards;
  const ruffs = ['n', 'e', 's', 'w'].map((x) => num(`ruffs-${x}`) ?? 0);
  if (ruffs.some(Boolean)) c.ruffs = ruffs;
  return c;
}

function writeConstraints(c) {
  const set = (field, v) => { id(field).value = v ?? ''; };
  set('vacant-e', c.vacant?.[0]);
  set('vacant-w', c.vacant?.[1]);
  set('known-e', c.known?.[0]);
  set('known-w', c.known?.[1]);
  for (const k of ['length', 'hcp']) {
    set(`${k}-e`, c[k] && rangeText(c[k][0], RANGE[k]));
    set(`${k}-w`, c[k] && rangeText(c[k][1], RANGE[k]));
  }
  set('first', c.first);
  set('entries-n', c.entries?.[0]);
  set('entries-s', c.entries?.[1]);
  set('rounds', c.rounds);
  set('lead', c.lead && RANKS[c.lead - 2]);
  set('discards', c.discards);
  ['n', 'e', 's', 'w'].forEach((x, i) => set(`ruffs-${x}`, c.ruffs?.[i] || ''));
  id('constraints').open = Object.keys(c).length > 0;
}

// --- sharing ------------------------------------------------------------------

function problem() {
  const j = job;
  return { n: j.n, s: j.s, ...(Object.keys(j.c).length ? { c: j.c } : {}), goal: view.goal, tab: view.tab, scoring: view.scoring };
}

function readHash() {
  try {
    const p = JSON.parse(decodeURIComponent(location.hash.slice(1)));
    return typeof p?.n === 'string' && typeof p?.s === 'string' ? p : null;
  } catch {
    return null;
  }
}

function download() {
  const j = job;
  const data = { problem: problem(), layouts: j.layouts, results: Object.fromEntries(j.results) };
  const a = document.createElement('a');
  a.href = URL.createObjectURL(new Blob([JSON.stringify(data, null, 1)], { type: 'application/json' }));
  a.download = `${j.n || '-'}-${j.s || '-'}.json`;
  a.click();
  URL.revokeObjectURL(a.href);
}

// --- views ------------------------------------------------------------------

function show() {
  const j = job;
  const ready = Boolean(j?.layouts);
  id('tabs').classList.toggle('hidden', !ready);
  id('out').classList.toggle('hidden', !ready);
  status();
  if (!ready) return;
  for (const b of document.querySelectorAll('#tabs [data-tab]')) b.classList.toggle('active', b.dataset.tab === view.tab);
  // Typing in the scoring inputs is not interrupted by results arriving
  if (id('out').contains(document.activeElement) && document.activeElement.matches('input')) return;
  id('out').innerHTML = view.tab === 'targets' ? targetsView(j) : linesView(j);
  history.replaceState(null, '', `#${encodeURIComponent(JSON.stringify(problem()))}`);
}

function targetsView(j) {
  const chips = j.keys.filter((k) => k.startsWith('t'))
    .map((k) => `<button class="chip ${k === view.goal ? 'active' : ''}" data-goal="${k}">${goalStatus(j, k)}</button>`)
    .join('');
  const a = j.results.get(view.goal);
  return `<div class="chips">${chips}</div>`
    + (finished(a) ? analysisView(j, a) + layoutsTable(j, [['Makes', a.guarantee]], a.target) : '');
}

function leadsTable(a) {
  const best = Math.max(...a.leads.map((l) => l.score));
  const rows = [...a.leads].sort((x, y) => y.score - x.score)
    .map((l) => `<tr class="${l.score === best ? 'best' : ''}"><td>${l.seat}: ${cardsText(l.cards)}</td><td class="num">${value(l.score, a.total)}</td></tr>`)
    .join('');
  return `<div class="panel-title">Every first card</div><table class="leads">${rows}</table>`;
}

// A line's words, its tree, and the explorer; `cmp` and `a`, line `a` of
// a comparison, let declarer's cards there be weighed
function lineView(j, plan, cmp = null, a = null) {
  return `<div class="panel-title">The line</div><p class="summary">${headline(plan, j.n, j.s)}</p>
    <details class="tree"><summary>Every branch</summary><div class="plan"><ul>${planItems(plan, j.n, j.s)}</ul></div></details>
    ${explorer(j, plan, cmp, a)}`;
}

function analysisView(j, a) {
  return leadsTable(a) + lineView(j, a.plan);
}

// West's (or East's) share of a layout: whole groups as cards, else how many
// of a group of interchangeable cards
function side(layout, west) {
  const text = layout.west.map(([h, n]) => {
    const k = west ? n : h.length - n;
    const cards = [...h].map(cardText).join('');
    return k === 0 ? '' : k === h.length ? cards : `${k} of ${cards}`;
  }).filter(Boolean).join(' ');
  return text || '—';
}

// Every layout the constraints allow, with what each column guarantees on it
function layoutsTable(j, columns, target) {
  const t = total(j);
  const count = (l) => l.west.reduce((s, [, n]) => s + n, 0);
  const rows = j.layouts.map((l, i) => ({ l, i })).filter(({ l }) => l.weight > 0)
    .sort((x, y) => count(x.l) - count(y.l) || y.l.weight - x.l.weight);
  const cell = (g) => (target !== undefined ? (g >= target ? '✓' : '✗') : g);
  const body = rows.map(({ l, i }) => {
    const cells = columns.map(([, g]) => cell(g[i]));
    return `<tr class="${new Set(cells).size > 1 ? 'diff' : ''}"><td>${side(l, true)}</td><td>${side(l, false)}</td><td class="num">${pct(l.weight, t)}</td>${cells.map((c) => `<td class="num">${c}</td>`).join('')}</tr>`;
  }).join('');
  return `<details class="layouts"><summary>Every layout (${rows.length})</summary>
    <table class="grid"><tr><th>West</th><th>East</th><th class="num">Odds</th>${columns.map(([h]) => `<th class="num">${h}</th>`).join('')}</tr>${body}</table></details>`;
}

function scoringControls() {
  const sc = view.scoring;
  const imps = sc.kind === 'imps';
  return `<div class="controls scoring">
    <label>Compare at <select id="sc-kind"><option value="matchpoints" ${imps ? '' : 'selected'}>matchpoints</option><option value="imps" ${imps ? 'selected' : ''}>IMPs</option></select></label>
    ${imps ? `<label>in <input id="sc-contract" size="5" value="${sc.contract}"></label>
      <label>making with <input id="sc-need" size="2" value="${sc.need}"> tricks in the suit</label>
      <label><input type="checkbox" id="sc-vul" ${sc.vulnerable ? 'checked' : ''}> vulnerable</label>` : ''}
  </div>`;
}

function readScoring() {
  if (id('sc-kind').value === 'matchpoints') return { kind: 'matchpoints' };
  const old = view.scoring;
  return {
    kind: 'imps',
    contract: id('sc-contract')?.value.trim() || old.contract || '3NT',
    need: Number(id('sc-need')?.value) || old.need || Math.max(1, job.max - 1),
    vulnerable: id('sc-vul')?.checked ?? false,
  };
}

// What line `a` of comparison `c` wins over line `b`, summed by weight
const gainOf = (c, a, b) => c.lines[a].guarantee
  .reduce((sum, x, i) => sum + c.weights[i] * c.payoff[x][c.lines[b].guarantee[i]], 0);

// Line `a`'s plan, asked of the worker once it is shown: building a plan
// for every line takes far longer than the comparison
function linePlan(j, cmp, a) {
  cmp.plans ??= [];
  if (cmp.plans[a] === undefined) {
    cmp.plans[a] = null;
    call('line', [a]).then(JSON.parse, (e) => ({ error: e.message }))
      .then((p) => { cmp.plans[a] = p; if (job === j) show(); });
  }
  return cmp.plans[a];
}

function linesView(j) {
  const cmp = j.results.get('cmp');
  let html = `<div class="panel-title">The best line and the best with each other first card</div>${scoringControls()}`;
  if (!finished(cmp)) return `${html}<p class="flag">${goalStatus(j, 'cmp')}: no line found yet</p>`;
  const t = cmp.total;
  const a = Math.min(view.a, cmp.lines.length - 1);
  const b = view.b !== null && view.b < cmp.lines.length && view.b !== a ? view.b : null;
  const imps = cmp.scoring.kind === 'imps';
  const gain = (w) => (imps ? `${(w / t).toFixed(3)}` : pct(w, t));
  const ks = Array.from({ length: j.max }, (_, i) => i + 1);
  const atLeast = (l, k) => l.tricks.slice(k).reduce((s, w) => s + w, 0);
  // A mix plays each of its lines as often as its weight
  const mixed = cmp.best.length > 1;
  const weight = cmp.mix.reduce((s, w) => s + w, 0);
  const often = (i) => pct(cmp.mix[i], weight);
  const rows = cmp.lines.map((l, i) => `<tr data-line="${i}" class="${i === a ? 'chosen' : ''}">
      <td>${cmp.best.includes(i) ? `★${mixed ? ` ${often(i)}` : ''}` : ''}</td><td>${i}</td><td>${l.seat[0]} ${cardsText(l.cards)}</td>
      ${ks.map((k) => `<td class="num">${pct(atLeast(l, k), t)}</td>`).join('')}
      <td class="num">${i === a ? '' : `+${gain(gainOf(cmp, i, a))} −${gain(gainOf(cmp, a, i))}`}</td>
      <td>${i === a ? '' : `<button class="mini ${i === b ? 'active' : ''}" data-versus="${i}">vs</button>`}</td></tr>`).join('');
  const at = imps ? 'IMPs' : 'matchpoints';
  const mix = `a mix of ★ ${cmp.best.map((i) => `line ${i} ${often(i)}`).join(', ')} of the time`;
  const tiny = cmp.slack / weight / t;
  const slack = imps ? tiny.toPrecision(2) : `${(tiny * 100).toPrecision(2)}%`;
  const verdict = !mixed ? (cmp.proven ? `★ Line 0 is the best at ${at}: no line beats it.`
    : cmp.open ? `★ Line 0 is the best found so far at ${at}; still checking whether any line beats it.`
    : `★ Line 0 is the best found at ${at}, not proven.`)
    : `No single best line at ${at}: lines beat each other in a cycle.  `
      + (cmp.proven ? `The best is ${mix}: no line gains over it (by more than ${slack} ${imps ? 'IMPs per deal' : 'of deals'}, for rounding).`
      : cmp.open ? `The best found so far is ${mix}; still checking whether any line gains over it.`
      : `The best found is ${mix}, not proven.`);
  html += `<p class="${cmp.proven ? 'note' : 'flag'}">${verdict}  The others are the best found with each other first card.  Click a line to see it.</p>
    <div class="scroll"><table class="grid lines"><tr><th></th><th>#</th><th>First</th>${ks.map((k) => `<th class="num">≥${k}</th>`).join('')}<th class="num">vs line ${a}${imps ? ' (IMPs)' : ''}</th><th></th></tr>${rows}</table></div>`;
  const line = cmp.lines[a];
  const dist = line.tricks.map((w, k) => (w ? `${k}: ${pct(w, t)}` : '')).filter(Boolean).join(' · ');
  html += `<div class="panel-title">Line ${a}</div><p>Tricks taken: ${dist}</p>`;
  if (b !== null) html += `<p>Line ${a} wins ${gain(gainOf(cmp, a, b))} ${imps ? 'IMPs per deal' : 'of deals'} over line ${b} and loses ${gain(gainOf(cmp, b, a))}.</p>`;
  const plan = linePlan(j, cmp, a);
  if (plan === null) html += '<p class="note">Building the line…</p>';
  else if (plan.error !== undefined) html += `<p class="flag">${plan.error}</p>`;
  else html += lineView(j, plan, cmp, a);
  const columns = [[`Line ${a}`, line.guarantee]];
  if (b !== null) columns.push([`Line ${b}`, cmp.lines[b].guarantee]);
  return html + layoutsTable(j, columns);
}

// --- the play explorer --------------------------------------------------------

// Plays `plan` along `path`, the defenders' cards at each choice (a rank,
// null for showing out, 'ruff'), to where it ends or the next choice
function walk(plan, north, south, path) {
  const out = missing(north, south);
  const hands = { North: north, South: south };
  const steps = [];
  let trick = [];
  let node = plan;
  let k = 0;
  for (;;) {
    if (node.kind === 'declarer' || node.kind === 'ruff') {
      const { text, card } = describe(node, trick, out);
      if (node.cards) hands[node.seat] = hands[node.seat].replace(node.cards.at(-1), '');
      steps.push({ seat: node.seat, card: node.kind === 'ruff' ? 'ruff' : node.cards ? cardsText(node.cards) : '–', text, raw: card, declarer: true });
      trick = advance(trick, card);
      node = node.then;
    } else if (node.kind === 'defender') {
      const choice = node.branches.length > 1 || Boolean(node.ruff);
      if (choice && k >= path.length) return { steps, node, hands, choice };
      let card;
      let sub;
      let shown;
      if (!choice) {
        const [labels] = node.branches[0];
        [card, sub, shown] = [pick(labels), node.branches[0][1], labels.map(rankText).join('/')];
      } else {
        card = path[k++];
        sub = card === 'ruff' ? node.ruff : node.branches.find(([labels]) => labels.includes(card))[1];
        shown = card === 'ruff' ? 'ruff' : rankText(card);
      }
      out.delete(card);
      steps.push({ seat: node.seat, card: shown, raw: card });
      trick = advance(trick, card);
      node = sub;
    } else {
      return { steps, node, hands, choice: false };
    }
  }
}

function explorer(j, plan, cmp = null, a = null) {
  const { steps, node, hands, choice } = walk(plan, j.n, j.s, view.path);
  const tricks = [];
  for (let i = 0; i < steps.length; i += 4) tricks.push(steps.slice(i, i + 4).map((s, k) => ({ ...s, i: i + k })));
  // In a comparison, declarer's cards open what each card there is worth
  const card = (s) => (cmp && s.declarer
    ? `<button class="mini ${s.i === view.at ? 'active' : ''}" data-at="${s.i}">${s.seat[0]} ${s.card}</button>`
    : `${s.seat[0]} ${s.card}`);
  const list = tricks.map((t) => `<li>${t.map(card).join(' · ')}
    <span class="words">${t.filter((s) => s.text).map((s) => s.text).join('; ')}</span></li>`).join('');
  const weighed = cmp && steps[view.at]?.declarer ? choicesView(j, cmp, a, steps.slice(0, view.at)) : '';
  let next;
  if (choice) {
    const cards = node.branches.flatMap(([labels]) => labels)
      .map((l) => `<button class="card small" data-pick="${JSON.stringify(l)}">${l === null ? 'out' : rankText(l)}</button>`);
    if (node.ruff) cards.push('<button class="card small" data-pick="&quot;ruff&quot;">ruff</button>');
    next = `${node.seat} to play: <span class="cards">${cards.join('')}</span>`;
  } else if (node.kind === 'made') {
    next = view.tab === 'targets' ? '<span class="made">✓ made</span>' : '<span class="made">✓ every trick played for is in</span>';
  } else {
    next = '<span class="hopeless">the line gives up here: no layout still played for reaches it</span>';
  }
  return `<div class="panel-title">Explore the line</div>
    <p class="hands">North ${[...hands.North].map(cardText).join('') || '—'} · South ${[...hands.South].map(cardText).join('') || '—'}</p>
    <ol class="tricks">${list}</ol>${weighed}
    <p class="next">${next}</p>
    <p>${view.path.length ? '<button class="secondary dark" data-back>Back</button> <button class="secondary dark" data-restart>Restart</button>' : ''}</p>`;
}

// Every card declarer may play after `before` (steps of line `a`), against
// the line's own: asked of the worker once, as the comparison's plans are
function choicesView(j, cmp, a, before) {
  const plays = before.map((s) => s.raw);
  if (plays.includes('ruff')) return '<p class="flag">Cards are not weighed after a ruff.</p>';
  const key = `${a}:${JSON.stringify(plays)}`;
  cmp.choices ??= {};
  if (!(key in cmp.choices)) {
    cmp.choices[key] = null;
    call('choices', [a, JSON.stringify(plays)], CHOICES).then(JSON.parse, (e) => ({ error: e.message }))
      .then((r) => { cmp.choices[key] = r; if (job === j) show(); });
  }
  const r = cmp.choices[key];
  if (r === null) return '<p class="note">Weighing every card here…</p>';
  if (r.error !== undefined) return `<p class="flag">${r.error}</p>`;
  const imps = cmp.scoring.kind === 'imps';
  const worth = (c) => {
    const x = c.net / r.total;
    const v = imps ? `${x >= 0 ? '+' : '−'}${Math.abs(x).toFixed(3)} IMPs` : `${x >= 0 ? '+' : '−'}${Math.abs(x * 100).toFixed(2)}%`;
    return c.exact ? v : `${v} or better`;
  };
  const rows = [...r.choices].sort((x, y) => y.net - x.net)
    .map((c) => `<tr class="${c.line ? 'chosen' : ''}"><td>${c.seat}: ${c.cards ? cardsText(c.cards) : 'discard'}${c.line ? ' (the line)' : ''}</td><td class="num">${worth(c)}</td></tr>`)
    .join('');
  return `<p class="note">Each card here, then the best way on, in place of the line's, against the line
    (${imps ? 'IMPs per deal' : 'of all deals'}); the defenders still play as they would against the line elsewhere.</p>
    <table class="leads">${rows}</table>`;
}

// --- wiring -----------------------------------------------------------------

function analyse() {
  try {
    const n = normalize(id('north').value);
    const s = normalize(id('south').value);
    start({ n, s, c: readConstraints(), goal: view.goal, tab: view.tab, scoring: view.scoring });
  } catch (e) {
    info(e.message, true);
  }
}

function main() {
  spawn();
  id('go').onclick = analyse;
  for (const el of document.querySelectorAll('main > .controls input, #constraints input')) {
    el.onkeydown = (event) => { if (event.key === 'Enter') analyse(); };
  }
  // One state's merge can hold a slice for long: past HARD_STOP the
  // worker goes, its memo with it.
  id('cancel').onclick = () => {
    const j = job;
    if (!j?.running) return;
    j.stop = true;
    setTimeout(() => { if (job === j && j.running && pending.size) restart('cancelled'); }, HARD_STOP);
  };
  id('more').onclick = () => job && !job.running && run(job);
  id('share').onclick = () => navigator.clipboard.writeText(location.href).then(() => info('Link copied'), () => info(location.href));
  id('json').onclick = download;
  for (const b of document.querySelectorAll('#tabs [data-tab]')) {
    b.onclick = () => {
      view.tab = b.dataset.tab;
      view.path = [];
      view.at = null;
      show();
    };
  }
  id('out').onclick = (e) => {
    const t = e.target.closest('[data-goal],[data-pick],[data-back],[data-restart],[data-versus],[data-line],[data-at]');
    if (!t) return;
    const d = t.dataset;
    if (d.goal) {
      view.goal = d.goal;
      view.path = [];
    } else if (d.pick !== undefined) {
      view.path.push(JSON.parse(d.pick));
      view.at = null;
    } else if (d.back !== undefined) {
      view.path.pop();
      view.at = null;
    } else if (d.restart !== undefined) {
      view.path = [];
      view.at = null;
    } else if (d.at !== undefined) view.at = Number(d.at) === view.at ? null : Number(d.at);
    else if (d.versus) view.b = Number(d.versus) === view.b ? null : Number(d.versus);
    else if (d.line) {
      view.a = Number(d.line);
      view.path = [];
      view.at = null;
    }
    show();
  };
  id('out').onchange = (e) => {
    if (!e.target.id.startsWith('sc-')) return;
    view.scoring = readScoring();
    job.results.delete('cmp');
    e.target.blur();
    if (!job.running) run(job);
    show();
  };
  // A link pasted into this tab changes only the hash
  window.onhashchange = openHash;
  openHash();
}

function openHash() {
  const p = readHash();
  if (!p || (job && JSON.stringify(p) === JSON.stringify(problem()))) return;
  id('north').value = p.n;
  id('south').value = p.s;
  writeConstraints(p.c ?? {});
  start(p);
}

main();
