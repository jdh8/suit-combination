// Thin UI over the wasm solver: one `analyze(north, south)` per problem
// returns every goal's optimal line; JS picks an interesting goal, shows the
// two hands, and grades the first card the user leads.
import init, { analyze } from './pkg/suit_combination_web.js';

const TOTAL = 10400600; // C(26, 13), the denominator of every score
const RANKS = '23456789TJQKA'; // index + 2 = rank value used in the JSON
const SUITS = [['♠', 's-s'], ['♥', 's-h'], ['♦', 's-d'], ['♣', 's-c']];
const id = (x) => document.getElementById(x);

let problem = null; // { north, south, suit, analysis, all }

async function main() {
  await init();
  id('new').onclick = newProblem;
  id('solve').onclick = customProblem;
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

function newProblem() {
  const lengths = id('length').value.split(',').map(Number);
  const kind = id('goalkind').value;
  for (let attempt = 0; attempt < 200; attempt++) {
    const [north, south] = randomHoldings(lengths[Math.floor(Math.random() * lengths.length)]);
    const all = JSON.parse(analyze(north, south));
    const goals = all.filter((a) => (kind === 'any' || a.goal.kind === kind) && interesting(a));
    if (goals.length === 0) continue;
    setProblem(north, south, all, goals[Math.floor(Math.random() * goals.length)]);
    return;
  }
  info('Could not find an interesting problem with these settings.', true);
}

// Accepts "aj52", "A J 10 2", "AJT2"; returns ranks in descending order.
function normalize(text) {
  const seen = new Set();
  for (const tok of text.toUpperCase().replace(/10/g, 'T').match(/[2-9TJQKA]/g) || []) seen.add(tok);
  return fmt([...seen].map((c) => RANKS.indexOf(c) + 2));
}

function customProblem() {
  const north = normalize(id('c-north').value);
  const south = normalize(id('c-south').value);
  const goalText = id('c-goal').value.trim().toLowerCase();
  let all;
  try {
    all = JSON.parse(analyze(north, south));
  } catch (e) {
    info(String(e), true);
    return;
  }
  let analysis;
  if (goalText === '' || goalText.startsWith('e')) analysis = all.find((a) => a.goal.kind === 'expected');
  else analysis = all.find((a) => a.goal.kind === 'tricks' && a.goal.target === Number(goalText));
  if (!analysis) {
    info(`No such goal: ${goalText}`, true);
    return;
  }
  setProblem(north, south, all, analysis);
}

function setProblem(north, south, all, analysis) {
  const suit = SUITS[Math.floor(Math.random() * SUITS.length)];
  problem = { north, south, suit, analysis, all };
  info('');
  renderTable();
  id('feedback').classList.add('hidden');
}

function info(text, error = false) {
  id('info').textContent = text;
  id('info').classList.toggle('error', error);
}

// --- rendering --------------------------------------------------------------

const seatName = { North: 'North', South: 'South' };

function goalText(goal) {
  return goal.kind === 'tricks'
    ? `How do you play for ${goal.target} trick${goal.target === 1 ? '' : 's'}?`
    : 'How do you play for the most tricks on average?';
}

function value(goal, score) {
  return goal.kind === 'tricks' ? `${(score / TOTAL * 100).toFixed(1)}%` : `${(score / TOTAL).toFixed(2)} tricks`;
}

function renderTable() {
  const { north, south, suit, analysis } = problem;
  const el = id('table');
  el.innerHTML = '';
  for (const [seat, cards] of [['North', north], ['South', south]]) {
    const row = document.createElement('div');
    row.className = 'hand';
    row.innerHTML = `<span class="label">${seat}</span><span class="suit ${suit[1]}">${suit[0]}</span>`;
    const box = document.createElement('span');
    box.className = 'cards';
    for (const rank of cards) {
      const b = document.createElement('button');
      b.className = 'card';
      b.textContent = rank === 'T' ? '10' : rank;
      b.dataset.seat = seat;
      b.dataset.rank = rank;
      b.onclick = () => grade(seat, rank);
      box.appendChild(b);
    }
    row.appendChild(box);
    el.appendChild(row);
  }
  const q = document.createElement('p');
  q.className = 'question';
  q.textContent = goalText(analysis.goal) + '  Click the card to lead.';
  el.appendChild(q);
}

function grade(seat, rank) {
  const { analysis, all } = problem;
  const chosen = analysis.leads.find((l) => l.seat === seat && l.cards.includes(rank));
  const best = Math.max(...analysis.leads.map((l) => l.score));
  for (const b of document.querySelectorAll('.card')) {
    b.disabled = true;
    const lead = analysis.leads.find((l) => l.seat === b.dataset.seat && l.cards.includes(b.dataset.rank));
    b.classList.toggle('best', lead.score === best);
    b.classList.toggle('chosen', lead === chosen);
  }
  const fb = id('feedback');
  fb.classList.remove('hidden');
  const ok = chosen.score === best;
  const rows = [...analysis.leads]
    .sort((a, b) => b.score - a.score)
    .map((l) => `<tr class="${l.score === best ? 'best' : ''} ${l === chosen ? 'chosen' : ''}">
      <td>${seatName[l.seat]}: ${cardsText(l.cards)}</td><td class="num">${value(analysis.goal, l.score)}</td></tr>`)
    .join('');
  const goals = all.map((a) => `<span class="${a === analysis ? 'active' : ''}">${a.goal.kind === 'tricks' ? `≥${a.goal.target}: ` : 'EV: '}${value(a.goal, a.score)}</span>`).join('');
  fb.innerHTML = `
    <p class="verdict ${ok ? 'ok' : 'no'}">${ok ? '✓ Best play' : '✗ Not the best play'} — ${value(analysis.goal, chosen.score)}${ok ? '' : `, best is ${value(analysis.goal, best)}`}</p>
    <div class="panel-title">Every lead</div>
    <table class="leads">${rows}</table>
    <div class="panel-title">Optimal line</div>
    <div class="plan"><ul>${planItems(analysis.plan, analysis.goal)}</ul></div>
    <div class="panel-title">This combination</div>
    <div class="goals">${goals}</div>
    <p style="margin:0.8rem 0 0"><button class="primary" id="next">Next problem</button></p>`;
  id('next').onclick = newProblem;
}

const cardsText = (cards) => [...cards].map((c) => (c === 'T' ? '10' : c)).join('/');

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
      return `<li>${seatName[plan.seat] ?? plan.seat}: ${what}</li>` + planItems(plan.then, goal);
    }
    case 'defender':
      if (plan.branches.length === 1) return planItems(plan.branches[0][1], goal);
      return plan.branches
        .map(([labels, sub]) => {
          const cards = labels.map((l) => (l === null ? 'nothing' : RANKS[l - 2] === 'T' ? '10' : RANKS[l - 2])).join('/');
          return `<li><span class="branch">${plan.seat} plays ${cards}:</span><ul>${planItems(sub, goal)}</ul></li>`;
        })
        .join('');
    default:
      return '';
  }
}

main();
