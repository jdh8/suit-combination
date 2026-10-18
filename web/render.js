// Shared by the trainer (app.js) and the analysis page (analyse.js): card
// text, values, and a plan tree told in bridge language.

export const RANKS = '23456789TJQKA'; // index + 2 = rank value used in the JSON
export const rankOf = (c) => RANKS.indexOf(c) + 2;
export const cardText = (c) => (c === 'T' ? '10' : c);
export const cardsText = (cards) => [...cards].map(cardText).join('/');
export const rankText = (r) => (r === null ? '–' : cardText(RANKS[r - 2]));
export const fmt = (ranks) => ranks.sort((a, b) => b - a).map((r) => RANKS[r - 2]).join('');

// Accepts "aj52", "A J 10 2", "AJT2"; returns ranks in descending order.
export function normalize(text) {
  const seen = new Set();
  for (const tok of text.toUpperCase().replace(/10/g, 'T').match(/[2-9TJQKA]/g) || []) seen.add(tok);
  return fmt([...seen].map(rankOf));
}

// `score` out of `weight`: a probability
export function value(score, weight) {
  return `${(score / weight * 100).toFixed(1)}%`;
}

// The defenders' cards: every rank North and South lack, as rank values
export const missing = (north, south) => new Set([...RANKS].filter((c) => !north.includes(c) && !south.includes(c)).map(rankOf));

// A trick so far is a list of cards: rank values, null for a discard or
// showing out, 'ruff'.  Defenders never lead, so declarer plays first and
// third.
export const advance = (trick, card) => (trick.length === 3 ? [] : [...trick, card]);

// The card standing for a defender's branch: its lowest, else showing out
// ponytail: a branch merging, say, the K and the 3 (same continuation) is
// told as if the 3 was played; tell each card apart if that misleads.
export const pick = (labels) => labels.filter((l) => l !== null).sort((a, b) => a - b)[0] ?? null;

// What declarer's `node` (a declarer or ruff step) does in bridge terms,
// given the `trick` so far and `out`, the defenders' cards not yet played.
// Returns the text and the card it adds to the trick.
export function describe(node, trick, out) {
  const seat = node.seat;
  if (node.kind === 'ruff') return { text: `${seat} ruffs`, card: 'ruff' };
  if (!node.cards) return { text: `${seat} discards`, card: null };
  const card = Math.max(...[...node.cards].map(rankOf));
  const c = `the ${cardsText(node.cards)}`;
  const tops = (r) => [...out].every((o) => o < r);
  if (trick.length === 0) return { text: tops(card) ? `${seat} cashes ${c}` : `${seat} leads ${c}`, card };
  const [lead, lho] = trick;
  if (trick.includes('ruff')) return { text: `${seat} plays ${c}`, card };
  const led = lead ?? 0;
  if (card > Math.max(led, lho ?? 0)) return { text: tops(card) ? `${seat} wins with ${c}` : `${seat} finesses ${c}`, card };
  if ((lho ?? 0) > led) return { text: `${seat} ducks, playing ${c}`, card };
  return { text: tops(led) ? `${seat} plays ${c}` : `${seat} plays ${c}, letting the ${rankText(led)} run`, card };
}

const labelText = (labels) => (labels.length === 1 && labels[0] === null ? 'shows out' : `plays ${labels.map((l) => (l === null ? 'nothing' : rankText(l))).join('/')}`);

function made() {
  return `<li class="made">✓ made</li>`;
}

function hopeless() {
  return `<li class="hopeless">cannot be made from here — play on for what it is worth</li>`;
}

// The plan as list items: declarer's steps as siblings, each defender
// choice opening one nested list per distinct continuation
export function planItems(plan, north, south) {
  return items(plan, [], missing(north, south));
}

function items(plan, trick, out) {
  switch (plan.kind) {
    case 'made':
      return made();
    case 'hopeless':
      return hopeless();
    case 'declarer':
    case 'ruff': {
      const { text, card } = describe(plan, trick, out);
      return `<li>${text}</li>` + items(plan.then, advance(trick, card), out);
    }
    case 'defender': {
      const follow = (labels, sub) => {
        const card = pick(labels);
        const rest = new Set(out);
        rest.delete(card);
        return items(sub, advance(trick, card), rest);
      };
      if (plan.branches.length === 1 && !plan.ruff) return follow(...plan.branches[0]);
      const branch = (what, inner) => `<li><span class="branch">${plan.seat} ${what}:</span><ul>${inner}</ul></li>`;
      return plan.branches.map(([labels, sub]) => branch(labelText(labels), follow(labels, sub))).join('')
        + (plan.ruff ? branch('ruffs', items(plan.ruff, advance(trick, 'ruff'), out)) : '');
    }
    default:
      return '';
  }
}

// The first trick of the plan in one sentence, following the defenders'
// lowest cards: "South leads the 2; if West plays 3/4, North finesses the Q."
export function headline(plan, north, south) {
  const out = missing(north, south);
  let trick = [];
  const parts = [];
  for (let node = plan, n = 0; node && n < 4; n++) {
    if (node.kind === 'declarer' || node.kind === 'ruff') {
      const { text, card } = describe(node, trick, out);
      parts.push(text);
      trick = advance(trick, card);
      node = node.then;
    } else if (node.kind === 'defender') {
      const [labels, sub] = [...node.branches].sort((a, b) => (pick(a[0]) ?? 99) - (pick(b[0]) ?? 99))[0];
      const card = pick(labels);
      if (node.branches.length > 1 || node.ruff) parts.push(`if ${node.seat} ${labelText(labels)}`);
      out.delete(card);
      trick = advance(trick, card);
      node = sub;
    } else {
      break;
    }
  }
  if (parts.at(-1)?.startsWith('if ')) parts.pop();
  // "if West plays 3/4" joins the step after it with a comma
  const text = parts.reduce((s, p, i) => s + (i === 0 ? '' : parts[i - 1].startsWith('if ') ? ', ' : '; ') + p, '');
  return text ? `${text[0].toUpperCase()}${text.slice(1)}.` : '';
}
