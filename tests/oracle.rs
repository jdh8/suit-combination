//! A plain, uncompressed small-position oracle for [`analyze`].
//!
//! It solves the engine's game with none of its compression: every concrete
//! layout of the missing cards is its own lane, every card is its own move
//! (no equivalence classes), and memo keys hold the actual cards seen.  It
//! shares no code with the engine; only the public API is compared.
//!
//! The game: declarer never sees the layout; the defenders see everything
//! and may play any card they hold (falsecards included), showing out only
//! when void.  Each trick is led by a declarer hand the [`Rules`] allow (by
//! default either, as with unlimited entries); a void declarer hand discards
//! in third seat.  The highest card wins the trick; only this suit's tricks
//! count.
//!
//! Each information state carries the Pareto antichain of "tricks per
//! layout" vectors that some pure declarer strategy guarantees from there:
//! where declarer moves, the union of the children's antichains; where a
//! defender moves, every way of picking one continuation per card, each
//! layout taking its worst legal card.
//!
//! [`analyze`]: suit_combination::analyze

use contract_bridge::{Holding, Rank, Seat};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use suit_combination::{Game, Goal, Solver, TOTAL};

/// Seats by index, as the trick goes round
const NORTH: usize = 0;
const EAST: usize = 1;
const SOUTH: usize = 2;
const WEST: usize = 3;

/// Ranks 2 to A as bits 2 to 14, like [`Holding::to_bits`]
const DECK: u16 = 0x7FFC;

/// Tricks declarer wins per layout still consistent with what declarer has
/// seen, in the order of [`Oracle::layouts`]
type Vector = Vec<u8>;

/// Between tricks, for North and South: the entry state after declarer
/// takes the lead in that hand, or `None` if it may not lead
type Access = [Option<u8>; 2];

/// The problem beyond the two holdings.  [`Rules::standard`] is the
/// engine's model; the fields are the hooks for constrained analysis.
struct Rules {
    /// Who may lead the next trick, from the seat that won the last one
    /// (`None` before the first trick) and the entry state, a count the rule
    /// keeps as it likes (unlimited entries: either hand, always)
    access: Box<dyn Fn(Option<usize>, u8) -> Access>,
    /// Entry state before the first trick
    entries: u8,
    /// Tricks played before the analysis stops
    rounds: u8,
    /// Weight of the layout in which West holds exactly these cards (bit
    /// `r` = rank `r`); 0 rules the layout out
    weight: Box<dyn Fn(u16) -> u64>,
}

impl Rules {
    /// Either hand leads every trick, the suit is played out, and layouts
    /// are weighted by 13/13 vacant places
    fn standard(north: Holding, south: Holding) -> Self {
        let missing = DECK & !north.to_bits() & !south.to_bits();
        Self {
            access: Box::new(|_, e| [Some(e); 2]),
            entries: 0,
            rounds: 13,
            weight: Box::new(move |west| vacant_places(missing, west)),
        }
    }
}

fn binom(n: u64, k: u64) -> u64 {
    (0..k).fold(1, |acc, i| acc * (n - i) / (i + 1))
}

/// Deals out of `C(26, 13)` in which West holds `west` of the `missing`
/// cards: `C(26 - n, 13 - w)`
fn vacant_places(missing: u16, west: u16) -> u64 {
    let n = u64::from(missing.count_ones());
    binom(26 - n, 13 - u64::from(west.count_ones()))
}

/// Everything declarer knows, which is all a strategy may depend on.  The
/// order of earlier plays tells nothing more: the defenders could have
/// played their cards in any order, and the tricks already won are the same
/// in every layout still possible.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Pos {
    /// Remaining cards of North, South
    hands: [u16; 2],
    /// Cards East, West have played
    played: [u16; 2],
    /// East, West have shown out
    out: [bool; 2],
    /// Seat that led the current trick (North between tricks)
    leader: usize,
    /// Rank each seat played to the current trick; 0 = discard or not yet
    trick: [u8; 4],
    /// Cards in the current trick
    count: usize,
    /// Hands that may lead the next trick; `[None; 2]` during a trick
    access: Access,
    /// Entry state during a trick ([`Rules::access`]); 0 between tricks
    entries: u8,
    /// Tricks completed
    round: u8,
}

/// Index into `hands` of a declarer seat, into `played` of a defender's
fn side(seat: usize) -> usize {
    seat / 2
}

fn cards(mask: u16) -> impl Iterator<Item = u8> {
    (2..15).filter(move |r| mask >> r & 1 != 0)
}

/// Every subset of `set`, as bit masks
fn subsets(set: u16) -> impl Iterator<Item = u16> {
    let mut next = Some(0u16);
    std::iter::from_fn(move || {
        let s = next?;
        next = (s != set).then(|| s.wrapping_sub(set) & set);
        Some(s)
    })
}

/// Drop duplicates and every vector another covers lane by lane: what
/// remains is a Pareto antichain
fn prune(vs: Vec<Vector>) -> Vec<Vector> {
    // Only a vector with at least the same sum can cover another, so sort
    // by sum, largest first, and compare each with those kept before it.
    let mut vs: Vec<(u32, Vector)> = vs
        .into_iter()
        .map(|v| (v.iter().map(|&t| u32::from(t)).sum(), v))
        .collect();
    vs.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    vs.dedup();
    let covers = |o: &Vector, v: &Vector| o.iter().zip(v).all(|(x, y)| x >= y);
    let mut out: Vec<Vector> = Vec::new();
    // The vector that covered the last one often covers the next.
    let mut last = 0;
    for (_, v) in vs {
        if out.get(last).is_some_and(|o| covers(o, &v)) {
            continue;
        }
        match out.iter().position(|o| covers(o, &v)) {
            Some(i) => last = i,
            None => out.push(v),
        }
    }
    out
}

struct Oracle {
    rules: Rules,
    north: u16,
    south: u16,
    /// West's cards in each layout of nonzero weight
    layouts: Vec<u16>,
    weights: Vec<u64>,
    memo: HashMap<Pos, Rc<Vec<Vector>>>,
    /// Most vectors pruned at once: past it [`solve`](Self::solve) gives up
    limit: usize,
}

impl Oracle {
    fn new(north: Holding, south: Holding, rules: Rules) -> Self {
        let (north, south) = (north.to_bits(), south.to_bits());
        let layouts: Vec<u16> = subsets(DECK & !north & !south)
            .filter(|&west| (rules.weight)(west) > 0)
            .collect();
        let weights = layouts.iter().map(|&w| (rules.weight)(w)).collect();
        Self {
            rules,
            north,
            south,
            layouts,
            weights,
            memo: HashMap::new(),
            limit: usize::MAX,
        }
    }

    fn missing(&self) -> u16 {
        DECK & !self.north & !self.south
    }

    /// Cards dealt to `seat` (East or West) in the layout `west`
    fn dealt(&self, seat: usize, west: u16) -> u16 {
        if seat == WEST {
            west
        } else {
            self.missing() & !west
        }
    }

    /// Layouts declarer cannot yet rule out: each defender was dealt the
    /// cards they played, and exactly those once they have shown out
    fn consistent(&self, p: &Pos) -> Vec<usize> {
        (0..self.layouts.len())
            .filter(|&i| {
                [EAST, WEST].iter().all(|&d| {
                    let dealt = self.dealt(d, self.layouts[i]);
                    let shown = p.played[side(d)];
                    dealt & shown == shown && !(p.out[side(d)] && dealt != shown)
                })
            })
            .collect()
    }

    fn root(&self) -> Pos {
        Pos {
            hands: [self.north, self.south],
            played: [0; 2],
            out: [false; 2],
            leader: NORTH,
            trick: [0; 4],
            count: 0,
            access: (self.rules.access)(None, self.rules.entries),
            entries: 0,
            round: 0,
        }
    }

    /// `seat` plays `rank` (0 = discards or shows out); the flag says
    /// whether declarer won a trick this completes
    fn play(&self, mut p: Pos, seat: usize, rank: u8) -> (Pos, bool) {
        if p.count == 0 {
            p.leader = seat;
            p.entries = p.access[side(seat)].expect("this hand may not lead");
            p.access = [None; 2];
        }
        assert_eq!(seat, (p.leader + p.count) % 4, "out of turn");
        if seat.is_multiple_of(2) {
            p.hands[side(seat)] &= !(1 << rank);
        } else if rank == 0 {
            p.out[side(seat)] = true;
        } else {
            p.played[side(seat)] |= 1 << rank;
        }
        p.trick[seat] = rank;
        p.count += 1;
        if p.count < 4 {
            return (p, false);
        }
        let winner = (0..4).max_by_key(|&s| p.trick[s]).unwrap();
        p.access = (self.rules.access)(Some(winner), p.entries);
        p.entries = 0;
        p.round += 1;
        p.leader = NORTH;
        p.trick = [0; 4];
        p.count = 0;
        (p, winner.is_multiple_of(2))
    }

    /// Declarer's options: (seat, rank played, resulting position).  Every
    /// card is its own move; a void hand discards in third seat and cannot
    /// lead.
    fn declarer_moves(&self, p: &Pos) -> Vec<(usize, u8, Pos)> {
        let seats: Vec<usize> = if p.count == 0 {
            [NORTH, SOUTH]
                .into_iter()
                .filter(|&s| p.access[side(s)].is_some())
                .collect()
        } else {
            vec![(p.leader + p.count) % 4]
        };
        let mut out = Vec::new();
        for seat in seats {
            let hand = p.hands[side(seat)];
            if hand == 0 && p.count != 0 {
                out.push((seat, 0, self.play(*p, seat, 0).0));
            }
            for r in cards(hand) {
                out.push((seat, r, self.play(*p, seat, r).0));
            }
        }
        out
    }

    /// [`prune`], or `None` for more vectors than the [`limit`](Self::limit)
    fn bounded(&self, vs: Vec<Vector>) -> Option<Vec<Vector>> {
        (vs.len() <= self.limit).then(|| prune(vs))
    }

    /// Pareto antichain of the tricks from here on, per consistent layout,
    /// that some declarer strategy guarantees, or `None` past the
    /// [`limit`](Self::limit).  Play stops between tricks after the last
    /// round, or when no hand allowed to lead has a card.
    fn solve(&mut self, p: Pos) -> Option<Rc<Vec<Vector>>> {
        if let Some(v) = self.memo.get(&p) {
            return Some(Rc::clone(v));
        }
        let out = if p.count == 0
            && (p.round == self.rules.rounds || self.declarer_moves(&p).is_empty())
        {
            vec![vec![0; self.consistent(&p).len()]]
        } else if p.count.is_multiple_of(2) {
            // Declarer's play reveals nothing: the children have our lanes.
            let mut all = Vec::new();
            for (_, _, ch) in self.declarer_moves(&p) {
                all.extend(self.solve(ch)?.iter().cloned());
            }
            self.bounded(all)?
        } else {
            self.defend(p, (p.leader + p.count) % 4)?
        };
        let out = Rc::new(out);
        self.memo.insert(p, Rc::clone(&out));
        Some(out)
    }

    /// [`solve`](Self::solve) where defender `seat` plays: each layout
    /// takes the branch worst for declarer among the cards it holds (or
    /// showing out), and declarer may answer each branch differently.
    fn defend(&mut self, p: Pos, seat: usize) -> Option<Vec<Vector>> {
        let consistent = self.consistent(&p);
        let unplayed = self.missing() & !p.played[0] & !p.played[1];
        // (our lanes where the card is legal, tricks it wins, child's antichain)
        let mut branches = Vec::new();
        for rank in cards(unplayed).chain([0]) {
            // Exactly the child's lanes, in the same order
            let legal: Vec<usize> = (0..consistent.len())
                .filter(|&j| {
                    let west = self.layouts[consistent[j]];
                    let held = self.dealt(seat, west) & !p.played[side(seat)];
                    if rank == 0 {
                        held == 0
                    } else {
                        held >> rank & 1 != 0
                    }
                })
                .collect();
            if legal.is_empty() {
                continue;
            }
            let (ch, won) = self.play(p, seat, rank);
            let branch = self.solve(ch)?;
            assert!(branch.iter().all(|y| y.len() == legal.len()));
            branches.push((legal, u8::from(won), branch));
        }
        // The result does not depend on the order; small antichains first
        // keep the partial products small.
        branches.sort_by_key(|b| b.2.len());
        let mut acc = vec![vec![u8::MAX; consistent.len()]];
        for (legal, bonus, branch) in branches {
            let mut next = Vec::new();
            for x in &acc {
                // Every result is at most x, and x itself is one when some
                // continuation covers x on this branch's lanes.
                if branch
                    .iter()
                    .any(|y| legal.iter().enumerate().all(|(m, &j)| y[m] + bonus >= x[j]))
                {
                    next.push(x.clone());
                    continue;
                }
                next.extend(prune(
                    branch
                        .iter()
                        .map(|y| {
                            let mut z = x.clone();
                            for (m, &j) in legal.iter().enumerate() {
                                z[j] = z[j].min(y[m] + bonus);
                            }
                            z
                        })
                        .collect(),
                ));
                if next.len() > self.limit {
                    return None;
                }
            }
            acc = self.bounded(next)?;
        }
        Some(acc)
    }

    /// Score of a vector over the root's layouts: deals reaching `target`,
    /// or tricks times deals
    fn score(&self, v: &Vector, target: Option<u8>) -> u64 {
        v.iter()
            .zip(&self.weights)
            .map(|(&t, &w)| match target {
                Some(k) => u64::from(t >= k) * w,
                None => u64::from(t) * w,
            })
            .sum()
    }

    /// Best score of any of `vs`
    fn best(&self, vs: &[Vector], target: Option<u8>) -> u64 {
        vs.iter().map(|v| self.score(v, target)).max().unwrap_or(0)
    }

    /// Deals consistent with the defenders having played `shown` (East,
    /// West) and shown out where `out` says
    fn deals(&self, shown: [u16; 2], out: [bool; 2]) -> u64 {
        let p = Pos {
            played: shown,
            out,
            ..self.root()
        };
        self.consistent(&p).iter().map(|&i| self.weights[i]).sum()
    }
}

/// The oracle solved from the root
struct Solved {
    oracle: Oracle,
    /// Antichain after each opening play: (seat, rank, vectors)
    leads: Vec<(usize, u8, Rc<Vec<Vector>>)>,
    /// Antichain of the root
    all: Vec<Vector>,
}

impl Solved {
    fn new(north: Holding, south: Holding) -> Self {
        Self::with(north, south, Rules::standard(north, south))
    }

    fn with(north: Holding, south: Holding, rules: Rules) -> Self {
        Self::within(Oracle::new(north, south, rules)).unwrap()
    }

    /// `oracle` solved, or `None` past its [`limit`](Oracle::limit)
    fn within(mut oracle: Oracle) -> Option<Self> {
        let root = oracle.root();
        let mut leads = Vec::new();
        for (seat, rank, ch) in oracle.declarer_moves(&root) {
            leads.push((seat, rank, oracle.solve(ch)?));
        }
        let all = prune(
            leads
                .iter()
                .flat_map(|(_, _, vs)| vs.iter().cloned())
                .collect(),
        );
        Some(Self { oracle, leads, all })
    }

    /// Best deals reaching `target`, or best tricks times deals
    fn best(&self, target: Option<u8>) -> u64 {
        self.oracle.best(&self.all, target)
    }

    /// Best score after leading `rank`, from whichever hand holds it
    fn lead(&self, rank: u8, target: Option<u8>) -> u64 {
        let (_, _, vs) = self.leads.iter().find(|l| l.1 == rank).unwrap();
        self.oracle.best(vs, target)
    }
}

fn seat_index(seat: Seat) -> usize {
    match seat {
        Seat::North => NORTH,
        Seat::East => EAST,
        Seat::South => SOUTH,
        Seat::West => WEST,
    }
}

/// A holding as text, `-` for a void
fn show(h: Holding) -> String {
    if h.is_empty() {
        "-".to_string()
    } else {
        h.to_string()
    }
}

fn name(north: Holding, south: Holding) -> String {
    format!("{} / {}", show(north), show(south))
}

/// Disagreements between the engine and the oracle, and how many numbers
/// were compared.  Every trick target and exact expected tricks must match
/// exactly, overall and per opening card; the fast expected-tricks line must
/// not beat the exact value (overall or per lead) and its bound must be the
/// sum of the targets.
fn compare(north: Holding, south: Holding, o: &Solved) -> (Vec<String>, usize) {
    let mut fails = Vec::new();
    let mut checks = 0;
    let name = name(north, south);
    let mut solver = Solver::new(north, south).unwrap();
    let longer = north.len().max(south.len()) as u8;
    let goals = (1..=longer)
        .map(|target| (Goal::Tricks { target }, Some(target)))
        .chain([(Goal::ExactExpected, None), (Goal::Expected, None)]);
    for (goal, target) in goals {
        let a = solver.analyze(goal).unwrap();
        let want = o.best(target);
        checks += 1;
        if goal == Goal::Expected {
            let targets: u64 = (1..=longer).map(|k| o.best(Some(k))).sum();
            checks += 1;
            if !(a.score <= want && want <= a.bound && a.bound == targets) {
                fails.push(format!(
                    "{name} Expected: line {} bound {} vs oracle exact {want}, targets {targets}",
                    a.score, a.bound
                ));
            }
        } else if a.score != want {
            fails.push(format!("{name} {goal:?}: engine {} oracle {want}", a.score));
        }
        let cards: usize = a.leads.iter().map(|l| l.cards.len()).sum();
        checks += 1;
        if cards != o.leads.len() {
            fails.push(format!(
                "{name} {goal:?}: engine leads {cards} cards, oracle {}",
                o.leads.len()
            ));
        }
        for (seat, rank, vs) in &o.leads {
            let want = o.oracle.best(vs, target);
            let card = Rank::new(*rank);
            checks += 1;
            match a
                .leads
                .iter()
                .find(|l| seat_index(l.seat) == *seat && l.cards.contains(card))
            {
                Some(l) if l.score == want || (goal == Goal::Expected && l.score <= want) => {}
                Some(l) => fails.push(format!(
                    "{name} {goal:?} lead {card}: engine {} oracle {want}",
                    l.score
                )),
                None => fails.push(format!("{name} {goal:?}: engine has no lead {card}")),
            }
        }
    }
    (fails, checks)
}

fn holding(s: &str) -> Holding {
    if s == "-" {
        Holding::EMPTY
    } else {
        s.parse().unwrap()
    }
}

/// Seeded random combinations, `min..=max` cards missing, `count` of them
/// without repeats; either hand may be void or a singleton
fn random_combos(count: usize, min: u32, max: u32, seed: u64) -> Vec<(Holding, Holding)> {
    // ponytail: xorshift64, so the sweep is the same every run
    let mut x = seed;
    let mut rand = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut out = Vec::new();
    while out.len() < count {
        let missing = min + (rand() % u64::from(max - min + 1)) as u32;
        let mut deck: Vec<u8> = (2..15).collect();
        for i in (1..deck.len()).rev() {
            deck.swap(i, (rand() % (i as u64 + 1)) as usize);
        }
        let declarer = &deck[..13 - missing as usize];
        let north_len = (rand() % (declarer.len() as u64 + 1)) as usize;
        let (n, s) = declarer.split_at(north_len);
        let bits =
            |ranks: &[u8]| Holding::from_bits_truncate(ranks.iter().fold(0, |b, r| b | 1 << r));
        let combo = (bits(n), bits(s));
        if !out.contains(&combo) {
            out.push(combo);
        }
    }
    out
}

/// Compare every combination, spread over the machine's threads, skipping
/// those the oracle gives up on at `limit` vectors; panics with every
/// disagreement
fn check_all(combos: &[(Holding, Holding)], limit: usize) {
    let short = |&(n, s): &(Holding, Holding)| n.len().min(s.len());
    assert!(combos.iter().any(|c| short(c) == 0), "no void hand");
    assert!(combos.iter().any(|c| short(c) == 1), "no singleton");
    let start = std::time::Instant::now();
    let next = AtomicUsize::new(0);
    let results = Mutex::new((Vec::new(), 0, 0, Vec::new()));
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                while let Some(&(n, s)) = combos.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let t = std::time::Instant::now();
                    let mut oracle = Oracle::new(n, s, Rules::standard(n, s));
                    oracle.limit = limit;
                    let Some(o) = Solved::within(oracle) else {
                        results.lock().unwrap().3.push(name(n, s));
                        continue;
                    };
                    let (fails, checks) = compare(n, s, &o);
                    if t.elapsed().as_secs() >= 5 {
                        eprintln!("slow: {} {:?}", name(n, s), t.elapsed());
                    }
                    let mut r = results.lock().unwrap();
                    r.0.extend(fails);
                    r.1 += checks;
                    r.2 += o.oracle.memo.len();
                }
            });
        }
    });
    let (fails, checks, states, skipped) = results.into_inner().unwrap();
    eprintln!(
        "{} combinations, {checks} numbers compared, {states} oracle states, {:?}",
        combos.len() - skipped.len(),
        start.elapsed()
    );
    if !skipped.is_empty() {
        eprintln!("skipped past {limit} vectors: {skipped:?}");
    }
    assert!(fails.is_empty(), "\n{}", fails.join("\n"));
}

#[test]
#[cfg_attr(debug_assertions, ignore = "slow unoptimized: run with --release")]
fn oracle_agrees_on_random_small_combinations() {
    check_all(&random_combos(120, 3, 6, 0x2545_F491_4F6C_DD1D), usize::MAX);
}

#[test]
#[ignore = "minutes: run with --release -- --ignored"]
fn oracle_agrees_on_more_combinations() {
    // ponytail: the oracle skips combinations whose antichains pass LIMIT
    // vectors (about 5%, some hours each); the engine goes unchecked there.
    const LIMIT: usize = 50_000;
    check_all(&random_combos(1000, 2, 8, 0x9E37_79B9_7F4A_7C15), LIMIT);
}

/// The reference cases with at most eight missing cards (today's engine
/// limit; each solves in under a second): the oracle agrees with the
/// engine, and on its own with the published values and plays
#[test]
#[cfg_attr(debug_assertions, ignore = "slow unoptimized: run with --release")]
fn oracle_agrees_on_reference_cases() {
    #[derive(serde::Deserialize)]
    struct Case {
        north: String,
        south: String,
        /// A trick target, or "expected"
        goal: serde_json::Value,
        score: Option<u64>,
        value: Option<f64>,
        tolerance: Option<f64>,
        leads: Option<Vec<String>>,
        avoid: Option<Vec<String>>,
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!("fixtures/cases.json")).unwrap();
    let mut solved: Vec<((Holding, Holding), Solved)> = Vec::new();
    let mut fails = Vec::new();
    for c in cases {
        let combo = (holding(&c.north), holding(&c.south));
        if 13 - combo.0.len() - combo.1.len() > 8 {
            continue;
        }
        if !solved.iter().any(|(k, _)| *k == combo) {
            let o = Solved::new(combo.0, combo.1);
            fails.extend(compare(combo.0, combo.1, &o).0);
            solved.push((combo, o));
        }
        let o = &solved.iter().find(|(k, _)| *k == combo).unwrap().1;
        let target = c.goal.as_u64().map(|t| u8::try_from(t).unwrap());
        assert!(target.is_some() || c.goal == "expected", "{}", c.goal);
        let name = format!("{} {target:?}", name(combo.0, combo.1));
        let best = o.best(target);
        if c.score.is_some_and(|s| s != best) {
            fails.push(format!("{name}: oracle {best}, fixture {:?}", c.score));
        }
        if let Some(value) = c.value {
            let tolerance = c.tolerance.unwrap_or(0.00005);
            let v = best as f64 / TOTAL as f64;
            if (v - value).abs() > tolerance {
                fails.push(format!("{name}: oracle {v:.5}, fixture {value}"));
            }
        }
        let rank = |r: &String| r.parse::<Rank>().unwrap().get();
        for r in c.leads.iter().flatten() {
            if o.lead(rank(r), target) != best {
                fails.push(format!("{name}: oracle finds lead {r} not optimal"));
            }
        }
        for r in c.avoid.iter().flatten() {
            if o.lead(rank(r), target) >= best {
                fails.push(format!("{name}: oracle finds lead {r} optimal"));
            }
        }
    }
    eprintln!("{} combinations", solved.len());
    assert!(fails.is_empty(), "\n{}", fails.join("\n"));
}

/// Values checked by hand, independent of the engine
#[test]
fn oracle_knows_simple_positions() {
    let best = |north: &str, south: &str, target: Option<u8>| {
        Solved::new(holding(north), holding(south)).best(target)
    };
    assert_eq!(best("AKQJT98765432", "-", Some(13)), TOTAL);
    // The finesse wins exactly when West holds the K: half the deals
    assert_eq!(best("AQ", "2", Some(2)) * 2, TOTAL);
    // AK opposite nothing: two tricks always, and no more
    assert_eq!(best("AK", "-", None), 2 * TOTAL);
    // AKJ opposite 32, three tricks: cash the A, dropping East's singleton
    // Q, then finesse the J, winning whenever West holds the Q
    let west_q: u64 = (0..=7u64)
        .map(|w| binom(7, w) * binom(18, 13 - (w + 1)))
        .sum();
    let east_stiff_q = binom(18, 13 - 7);
    assert_eq!(best("AKJ", "32", Some(3)), west_q + east_stiff_q);
}

/// The hooks for constrained analysis, on values checked by hand
#[test]
fn oracle_rules_hooks() {
    let (north, south) = (holding("AQ"), holding("2"));
    let missing = DECK & !north.to_bits() & !south.to_bits();
    let king = 1 << 13;

    // North must lead first: only a singleton K, on either side, falls
    let north_first = Rules {
        access: Box::new(|winner, e| [Some(e), winner.map(|_| e)]),
        ..Rules::standard(north, south)
    };
    let stiff = vacant_places(missing, king) + vacant_places(missing, missing & !king);
    assert_eq!(Solved::with(north, south, north_first).best(Some(2)), stiff);

    // West is known to hold the K: the finesse always wins
    let known = Rules {
        weight: Box::new(move |west| {
            if west & king == 0 {
                0
            } else {
                vacant_places(missing, west)
            }
        }),
        ..Rules::standard(north, south)
    };
    let o = Solved::with(north, south, known);
    let deals: u64 = o.oracle.weights.iter().sum();
    assert_eq!(deals * 2, TOTAL);
    assert_eq!(o.best(Some(2)), deals);

    // One round: AK opposite nothing takes one trick
    let (ak, void) = (holding("AK"), Holding::EMPTY);
    let one = Rules {
        rounds: 1,
        ..Rules::standard(ak, void)
    };
    assert_eq!(Solved::with(ak, void, one).best(None), TOTAL);

    // The hand that wins a trick leads the next: A opposite KQ is blocked
    let (a, kq) = (holding("A"), holding("KQ"));
    let winner_leads = Rules {
        access: Box::new(|winner, e| match winner {
            Some(NORTH) => [Some(e), None],
            Some(SOUTH) => [None, Some(e)],
            _ => [Some(e); 2],
        }),
        ..Rules::standard(a, kq)
    };
    assert_eq!(Solved::new(a, kq).best(Some(2)), TOTAL);
    let blocked = Solved::with(a, kq, winner_leads);
    assert_eq!((blocked.best(Some(1)), blocked.best(Some(2))), (TOTAL, 0));

    // Declarer starts in North and stays in the hand that won; reaching
    // South otherwise spends one of its `e` outside entries.  A opposite KQ
    // unblocks with one entry: A, then the K from South.
    let south_entries = |entries| Rules {
        access: Box::new(|winner, e: u8| match winner {
            Some(SOUTH) => [Some(e); 2],
            _ => [Some(e), e.checked_sub(1)],
        }),
        entries,
        ..Rules::standard(a, kq)
    };
    let tricks = |entries| Solved::with(a, kq, south_entries(entries)).best(None) / TOTAL;
    assert_eq!((tricks(0), tricks(1)), (1, 2));
}

/// [`Game::weight`], the denominator of [`Game::moves`]' scores, after every
/// declarer line of a small combination on every layout, against the deals
/// that hold the cards the defenders have shown
#[test]
#[ignore = "known issue, fixed in phase 4: Game::weight counts class binomials"]
fn game_weight_counts_the_deals_holding_the_cards_seen() {
    // Known issue: after a defender shows a card of a multi-card class, the
    // engine weighs a layout with West holding k of the class's m cards by
    // the class binomial C(m, k), not by the C(m - sW - sE, k - sW) layouts
    // holding the sW and sE cards West and East have shown.  For Q83 / T62
    // with West J74, after South 2, West 7, North 3, East 5 the engine says
    // 4,056,234 deals; brute force finds 2,704,156.  Root scores are
    // unaffected; Game::moves' mid-play scores use the same weights.
    let (north, south) = (holding("Q83"), holding("T62"));
    let oracle = Oracle::new(north, south, Rules::standard(north, south));
    let mut solver = Solver::new(north, south).unwrap();
    let goal = Goal::Tricks { target: 1 };
    let mut fails = Vec::new();

    // The documented example first
    let mut game = Game::new(&mut solver, holding("J74"), goal).unwrap();
    let mut plays = game.play(&mut solver, Rank::new(2)).unwrap();
    plays.extend(game.play(&mut solver, Rank::new(3)).unwrap());
    let shown: Vec<_> = plays
        .iter()
        .map(|p| (p.seat, p.card.map(Rank::get)))
        .collect();
    let expected = [
        (Seat::South, Some(2)),
        (Seat::West, Some(7)),
        (Seat::North, Some(3)),
        (Seat::East, Some(5)),
    ];
    assert_eq!(shown, expected, "the defenders changed their cards");
    let (engine, deals) = (
        game.weight(&mut solver),
        oracle.deals([1 << 5, 1 << 7], [false; 2]),
    );
    if engine != deals {
        fails.push(format!(
            "West J74, S2 W7 N3 E5: engine {engine}, deals {deals}"
        ));
    }

    // Then every declarer line on every layout
    fn walk(
        game: Game,
        seen: ([u16; 2], [bool; 2], String),
        solver: &mut Solver,
        oracle: &Oracle,
        fails: &mut Vec<String>,
    ) {
        for lead in game.moves(solver) {
            let rank = Rank::new(lead.cards.to_bits().trailing_zeros() as u8);
            let mut game = game.clone();
            let (mut shown, mut out, mut line) = seen.clone();
            for p in game.play(solver, rank).unwrap() {
                let seat = format!("{:?}", p.seat);
                let card = p.card.map_or("-".into(), |r| r.to_string());
                line += &format!(" {}{card}", &seat[..1]);
                let d = match p.seat {
                    Seat::East => 0,
                    Seat::West => 1,
                    _ => continue,
                };
                match p.card {
                    Some(r) => shown[d] |= 1 << r.get(),
                    None => out[d] = true,
                }
            }
            let (engine, deals) = (game.weight(solver), oracle.deals(shown, out));
            if engine != deals {
                fails.push(format!("{line}: engine {engine}, deals {deals}"));
            }
            walk(game, (shown, out, line), solver, oracle, fails);
        }
    }
    for &west in &oracle.layouts {
        let west = Holding::from_bits_truncate(west);
        let game = Game::new(&mut solver, west, goal).unwrap();
        let line = format!("West {}:", show(west));
        walk(
            game,
            ([0; 2], [false; 2], line),
            &mut solver,
            &oracle,
            &mut fails,
        );
    }
    let shown = fails
        .iter()
        .take(10)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    assert!(fails.is_empty(), "{} mismatches:\n{shown}", fails.len());
}
