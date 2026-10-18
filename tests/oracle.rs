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
use suit_combination::{Constraints, Error, Scoring, Solver, TOTAL};

/// Seats by index, as the trick goes round
const NORTH: usize = 0;
const EAST: usize = 1;
const SOUTH: usize = 2;
const WEST: usize = 3;

/// Ranks 2 to A as bits 2 to 14, like [`Holding::to_bits`]
const DECK: u16 = 0x7FFC;

/// A ruff in [`Pos::trick`]: it beats every card of the suit
const RUFF: u8 = 15;

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
    /// Card declarer must lead to the first trick
    lead: Option<u8>,
    /// Declarer's cards kept out of play, and out of the defenders' hands
    discards: u16,
    /// Rounds each seat may ruff once void
    ruffs: [u8; 4],
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
            lead: None,
            discards: 0,
            ruffs: [0; 4],
            weight: Box::new(move |west| vacant_places(missing, west)),
        }
    }

    /// [`standard`](Self::standard) with layouts weighted under `c`, and
    /// its first leader and entries
    fn constrained(north: Holding, south: Holding, c: Constraints) -> Self {
        let missing = DECK & !north.to_bits() & !south.to_bits();
        // Entry state: entries left to North in the low four bits, South in
        // the high four, 15 for unlimited
        let first = c.first.map(seat_index);
        let access = move |winner: Option<usize>, e: u8| {
            [NORTH, SOUTH].map(|hand| {
                let shift = 2 * hand; // 0 for North, 4 for South
                let left = e >> shift & 15;
                let free = match winner {
                    None => first.is_none_or(|f| f == hand),
                    Some(w) => w == hand,
                };
                if free || left == 15 {
                    Some(e)
                } else {
                    left.checked_sub(1).map(|l| e & !(15 << shift) | l << shift)
                }
            })
        };
        let nibble = |n: Option<u8>| n.map_or(15, |n| n.min(14));
        Self {
            access: Box::new(access),
            entries: nibble(c.entries[0]) | nibble(c.entries[1]) << 4,
            rounds: c.rounds,
            lead: c.lead.map(Rank::get),
            discards: c.discards.to_bits(),
            ruffs: c.ruffs,
            weight: Box::new(move |west| constrained(missing, west, c)),
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

/// Deals in which West holds exactly `west` of the `missing` cards, under
/// `c`: 0 unless each defender holds the cards known to be theirs, else the
/// ways to fill West's other vacant places from the defenders' unknown
/// cards of other suits
fn constrained(missing: u16, west: u16, c: Constraints) -> u64 {
    let [east_known, west_known] = c.known.map(Holding::to_bits);
    let (n, w) = (missing.count_ones(), west.count_ones());
    let [east_places, west_places] = c.vacant.map(u32::from);
    if west & west_known != west_known || west & east_known != 0 {
        return 0;
    }
    if w > west_places || n - w > east_places {
        return 0;
    }
    // Points of the jack up, by hand rather than the engine's helper
    let hcp = |m: u16| (1..=4).filter(|&p| m >> (10 + p) & 1 != 0).sum::<u32>();
    let length = [n - w, w];
    let points = [hcp(missing & !west), hcp(west)];
    let within = |[lo, hi]: [u8; 2], x: u32| u32::from(lo) <= x && x <= u32::from(hi);
    if (0..2).any(|d| !within(c.length[d], length[d]) || !within(c.hcp[d], points[d])) {
        return 0;
    }
    let unknown = east_places + west_places - n;
    binom(unknown.into(), (west_places - w).into())
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
    /// Ruffs left by seat
    ruffs: [u8; 4],
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
    /// Cards in play of North, South
    north: u16,
    south: u16,
    /// Cards East and West share: neither declarer's nor discarded
    missing: u16,
    /// West's cards in each layout of nonzero weight
    layouts: Vec<u16>,
    weights: Vec<u64>,
    memo: HashMap<Pos, Rc<Vec<Vector>>>,
    /// Most vectors pruned at once: past it [`solve`](Self::solve) gives up
    limit: usize,
}

impl Oracle {
    fn new(north: Holding, south: Holding, rules: Rules) -> Self {
        let missing = DECK & !north.to_bits() & !south.to_bits();
        let (north, south) = (
            north.to_bits() & !rules.discards,
            south.to_bits() & !rules.discards,
        );
        let layouts: Vec<u16> = subsets(missing)
            .filter(|&west| (rules.weight)(west) > 0)
            .collect();
        let weights = layouts.iter().map(|&w| (rules.weight)(w)).collect();
        Self {
            rules,
            north,
            south,
            missing,
            layouts,
            weights,
            memo: HashMap::new(),
            limit: usize::MAX,
        }
    }

    fn missing(&self) -> u16 {
        self.missing
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
            ruffs: self.rules.ruffs,
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
        if rank == RUFF {
            p.ruffs[seat] -= 1;
            if !seat.is_multiple_of(2) {
                p.out[side(seat)] = true;
            }
        } else if seat.is_multiple_of(2) {
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
    /// lead; the first lead may be fixed.
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
                if p.ruffs[seat] > 0 {
                    out.push((seat, RUFF, self.play(*p, seat, RUFF).0));
                }
            }
            for r in cards(hand) {
                let fixed = p.round == 0 && p.count == 0 && self.rules.lead.is_some();
                if !fixed || self.rules.lead == Some(r) {
                    out.push((seat, r, self.play(*p, seat, r).0));
                }
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
        for rank in cards(unplayed).chain([0, RUFF]) {
            // Exactly the child's lanes, in the same order
            let legal: Vec<usize> = (0..consistent.len())
                .filter(|&j| {
                    let west = self.layouts[consistent[j]];
                    let held = self.dealt(seat, west) & !p.played[side(seat)];
                    if rank == 0 {
                        held == 0
                    } else if rank == RUFF {
                        held == 0 && p.ruffs[seat] > 0
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
/// were compared.  Every trick target must match exactly, overall and per
/// opening card.
fn compare(north: Holding, south: Holding, c: Constraints, o: &Solved) -> (Vec<String>, usize) {
    let mut fails = Vec::new();
    let mut checks = 0;
    let name = format!("{} {c:?}", name(north, south));
    let mut solver = Solver::with_constraints(north, south, c).unwrap();
    let d = c.discards;
    let longer = ((north - d).len().max((south - d).len()) as u8).min(c.rounds);
    for goal in 1..=longer {
        let a = solver.analyze(goal).unwrap();
        let target = Some(goal);
        let want = o.best(target);
        checks += 1;
        if a.score != want {
            fails.push(format!("{name} {goal:?}: engine {} oracle {want}", a.score));
        }
        let cards: usize = a.leads.iter().map(|l| l.cards.len()).sum();
        checks += 1;
        // A fixed lead is one card to the oracle, its class to the engine
        if c.lead.is_none() && cards != o.leads.len() {
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
                Some(l) if l.score == want => {}
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
fn check_all(
    combos: &[(Holding, Holding)],
    limit: usize,
    constrain: impl Fn(Holding, Holding) -> Constraints + Sync,
) {
    let short = |&(n, s): &(Holding, Holding)| n.len().min(s.len());
    assert!(combos.iter().any(|c| short(c) == 0), "no void hand");
    assert!(combos.iter().any(|c| short(c) == 1), "no singleton");
    let start = std::time::Instant::now();
    let next = AtomicUsize::new(0);
    let results = Mutex::new((Vec::new(), 0, 0, Vec::new(), 0));
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                while let Some(&(n, s)) = combos.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let t = std::time::Instant::now();
                    let c = constrain(n, s);
                    // Over 256 layouts: only cards told apart by known
                    // cards or an HCP range can get there
                    if Solver::with_constraints(n, s, c).err() == Some(Error::TooManyMissing) {
                        let d = Constraints::default();
                        let mut r = results.lock().unwrap();
                        if c.known == d.known && c.hcp == d.hcp {
                            r.0.push(format!("{} {c:?}: too many layouts?", name(n, s)));
                        }
                        r.4 += 1;
                        continue;
                    }
                    let mut oracle = Oracle::new(n, s, Rules::constrained(n, s, c));
                    oracle.limit = limit;
                    if oracle.declarer_moves(&oracle.root()).is_empty() {
                        // No hand can lead: the engine must reject the problem
                        let mut r = results.lock().unwrap();
                        r.1 += 1;
                        if Solver::with_constraints(n, s, c).err() != Some(Error::BadConstraints) {
                            r.0.push(format!("{} {c:?}: engine accepts no lead", name(n, s)));
                        }
                        continue;
                    }
                    if oracle.layouts.is_empty() {
                        // No layout fits: the engine must say so
                        let mut r = results.lock().unwrap();
                        r.1 += 1;
                        if Solver::with_constraints(n, s, c).err() != Some(Error::NoLayout) {
                            r.0.push(format!("{} {c:?}: engine accepts no layout", name(n, s)));
                        }
                        continue;
                    }
                    let Some(o) = Solved::within(oracle) else {
                        results.lock().unwrap().3.push(name(n, s));
                        continue;
                    };
                    let (fails, checks) = compare(n, s, c, &o);
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
    let (fails, checks, states, skipped, layouts) = results.into_inner().unwrap();
    eprintln!(
        "{} combinations, {checks} numbers compared, {states} oracle states, {:?}",
        combos.len() - skipped.len() - layouts,
        start.elapsed()
    );
    if layouts > 0 {
        eprintln!("{layouts} past 256 layouts, rejected by the engine");
    }
    if !skipped.is_empty() {
        eprintln!("skipped past {limit} vectors: {skipped:?}");
    }
    assert!(fails.is_empty(), "\n{}", fails.join("\n"));
}

#[test]
#[cfg_attr(debug_assertions, ignore = "slow unoptimized: run with --release")]
fn oracle_agrees_on_random_small_combinations() {
    check_all(
        &random_combos(120, 3, 6, 0x2545_F491_4F6C_DD1D),
        usize::MAX,
        |_, _| Constraints::default(),
    );
}

/// Random vacant places from 4 to 13; for each missing card, one chance in
/// three that it is known to be with East or West; even chances of a random
/// length range and of a random HCP range for one defender; a random first
/// lead or none, and for each declarer hand even chances of 0 to 2
/// entries; one chance in three of a random limit on rounds, one in eight
/// that each declarer card is discarded, and one in four of a random
/// fixed lead; one chance in three each of 0 to 2 ruffs per declarer
/// hand or per defender; seeded by the combination; sometimes no layout
/// fits or no lead is legal
fn random_constraints(north: Holding, south: Holding) -> Constraints {
    let mut x = u64::from(north.to_bits()) << 16 | u64::from(south.to_bits()) | 1 << 40;
    let mut rand = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut known = [0u16; 2];
    for r in cards(DECK & !north.to_bits() & !south.to_bits()) {
        match rand() % 6 {
            0 => known[0] |= 1 << r,
            1 => known[1] |= 1 << r,
            _ => {}
        }
    }
    let vacant = [4 + (rand() % 10) as u8, 4 + (rand() % 10) as u8];
    let missing = DECK & !north.to_bits() & !south.to_bits();
    let mut c = Constraints {
        vacant,
        known: known.map(Holding::from_bits_truncate),
        ..Constraints::default()
    };
    let hcp = (11..15)
        .filter(|&r| missing >> r & 1 != 0)
        .map(|r| r - 10)
        .sum::<u64>();
    for (ranges, most) in [
        (&mut c.length, u64::from(missing.count_ones())),
        (&mut c.hcp, hcp),
    ] {
        if rand() % 2 == 0 {
            let lo = rand() % (most + 1);
            let hi = lo + rand() % (most + 1 - lo);
            ranges[(rand() % 2) as usize] = [lo as u8, hi as u8];
        }
    }
    c.first = [None, Some(Seat::North), Some(Seat::South)][(rand() % 3) as usize];
    for e in &mut c.entries {
        if rand() % 2 == 0 {
            *e = Some((rand() % 3) as u8);
        }
    }
    let declarer = north.to_bits() | south.to_bits();
    if rand() % 3 == 0 {
        c.rounds = 1 + (rand() % u64::from(north.len().max(south.len()).max(1) as u8)) as u8;
    }
    for r in cards(declarer) {
        if rand() % 8 == 0 {
            c.discards |= Holding::from_bits_truncate(1 << r);
        }
    }
    let left: Vec<u8> = cards(declarer & !c.discards.to_bits()).collect();
    if rand() % 4 == 0 && !left.is_empty() {
        c.lead = Some(Rank::new(left[(rand() % left.len() as u64) as usize]));
    }
    let side = rand() % 3;
    if side > 0 {
        for seat in [side as usize - 1, side as usize + 1] {
            c.ruffs[seat] = (rand() % 3) as u8;
        }
    }
    c
}

#[test]
#[cfg_attr(debug_assertions, ignore = "slow unoptimized: run with --release")]
fn oracle_agrees_on_many_missing_cards() {
    // ponytail: up to 4096 uncompressed lanes; the oracle skips the
    // combinations whose antichains pass LIMIT vectors
    const LIMIT: usize = 2_000;
    check_all(
        &random_combos(120, 9, 12, 0x6A09_E667_F3BC_C908),
        LIMIT,
        |_, _| Constraints::default(),
    );
    // Known cards and HCP ranges tell so many cards apart that most pass
    // 256 layouts; keep the rest of the constraints
    check_all(
        &random_combos(120, 9, 12, 0xBB67_AE85_84CA_A73B),
        LIMIT,
        |n, s| Constraints {
            known: Constraints::default().known,
            hcp: Constraints::default().hcp,
            ..random_constraints(n, s)
        },
    );
}

#[test]
#[cfg_attr(debug_assertions, ignore = "slow unoptimized: run with --release")]
fn oracle_agrees_under_constraints() {
    check_all(
        &random_combos(200, 3, 7, 0xD1B5_4A32_D192_ED03),
        usize::MAX,
        random_constraints,
    );
}

#[test]
#[ignore = "minutes: run with --release -- --ignored"]
fn oracle_agrees_on_more_combinations() {
    // ponytail: the oracle skips combinations whose antichains pass LIMIT
    // vectors (about 5%, some hours each); the engine goes unchecked there.
    const LIMIT: usize = 50_000;
    check_all(
        &random_combos(1000, 2, 8, 0x9E37_79B9_7F4A_7C15),
        LIMIT,
        |_, _| Constraints::default(),
    );
}

/// The reference cases: the oracle agrees with the engine, and on its own
/// with the published values and plays
#[test]
#[cfg_attr(debug_assertions, ignore = "slow unoptimized: run with --release")]
fn oracle_agrees_on_reference_cases() {
    #[derive(serde::Deserialize)]
    struct Case {
        north: String,
        south: String,
        goal: u8,
        vacant: Option<[u8; 2]>,
        east: Option<String>,
        west: Option<String>,
        length: Option<[[u8; 2]; 2]>,
        hcp: Option<[[u8; 2]; 2]>,
        first: Option<String>,
        entries: Option<[Option<u8>; 2]>,
        rounds: Option<u8>,
        lead: Option<String>,
        discard: Option<String>,
        ruffs: Option<[u8; 4]>,
        score: Option<u64>,
        value: Option<f64>,
        tolerance: Option<f64>,
        leads: Option<Vec<String>>,
        avoid: Option<Vec<String>>,
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!("fixtures/cases.json")).unwrap();
    let mut solved: Vec<((Holding, Holding, Constraints), Solved)> = Vec::new();
    let mut fails = Vec::new();
    for c in cases {
        let known = |h: &Option<String>| h.as_deref().map_or(Holding::EMPTY, holding);
        let d = Constraints::default();
        let constraints = Constraints {
            vacant: c.vacant.unwrap_or(d.vacant),
            known: [known(&c.east), known(&c.west)],
            length: c.length.unwrap_or(d.length),
            hcp: c.hcp.unwrap_or(d.hcp),
            first: c.first.as_deref().map(|s| match s {
                "N" => Seat::North,
                _ => Seat::South,
            }),
            entries: c.entries.unwrap_or(d.entries),
            rounds: c.rounds.unwrap_or(d.rounds),
            lead: c.lead.as_deref().map(|r| r.parse().unwrap()),
            discards: known(&c.discard),
            ruffs: c.ruffs.unwrap_or(d.ruffs),
        };
        let combo = (holding(&c.north), holding(&c.south), constraints);
        if !solved.iter().any(|(k, _)| *k == combo) {
            let o = Solved::with(
                combo.0,
                combo.1,
                Rules::constrained(combo.0, combo.1, combo.2),
            );
            fails.extend(compare(combo.0, combo.1, combo.2, &o).0);
            solved.push((combo, o));
        }
        let o = &solved.iter().find(|(k, _)| *k == combo).unwrap().1;
        let total: u64 = o.oracle.weights.iter().sum();
        let target = Some(c.goal);
        let name = format!("{} {target:?}", name(combo.0, combo.1));
        let best = o.best(target);
        if c.score.is_some_and(|s| s != best) {
            fails.push(format!("{name}: oracle {best}, fixture {:?}", c.score));
        }
        if let Some(value) = c.value {
            let tolerance = c.tolerance.unwrap_or(0.00005);
            let v = best as f64 / total as f64;
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

/// Before any target finishes, an interrupted analysis is bounded by
/// double dummy: on each layout alone, where declarer knows it, the oracle
/// says how many tricks that is.
#[test]
#[cfg_attr(debug_assertions, ignore = "slow unoptimized: run with --release")]
fn interrupted_bound_is_double_dummy() {
    let check = |north: Holding, south: Holding| {
        let missing = DECK & !north.to_bits() & !south.to_bits();
        let max = north.len().max(south.len()) as u8;
        // Deals by the tricks declarer takes double dummy
        let mut deals = vec![0; usize::from(max) + 1];
        for west in subsets(missing) {
            let weight = vacant_places(missing, west);
            let rules = Rules {
                weight: Box::new(move |w| if w == west { weight } else { 0 }),
                ..Rules::standard(north, south)
            };
            let tricks = Solved::with(north, south, rules).best(None) / weight;
            deals[tricks as usize] += weight;
        }
        for goal in 1..=max {
            let want: u64 = deals[usize::from(goal)..].iter().sum();
            let mut solver = Solver::new(north, south).unwrap();
            solver.set_poll_interval(1);
            solver.set_interrupt(|_| true);
            match solver.analyze(goal) {
                Err(Error::Interrupted { bound, .. }) => {
                    assert_eq!(bound, want, "{} {goal:?}", name(north, south));
                }
                // Nothing to search: no line beats double dummy either
                Ok(a) => assert!(a.score <= want, "{} {goal:?}", name(north, south)),
                Err(e) => panic!("{e}"),
            }
        }
    };
    std::thread::scope(|scope| {
        for (north, south) in random_combos(60, 3, 6, 0x5DEE_CE66_D1CE_4E5B) {
            scope.spawn(move || check(north, south));
        }
    });
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

/// IMPs for `points` in favour, by the WBF scale, written apart from the
/// engine's
fn imps(points: i32) -> u64 {
    let scale = [
        20, 50, 90, 130, 170, 220, 270, 320, 370, 430, 500, 600, 750, 900, 1100, 1300, 1500, 1750,
        2000, 2250, 2500, 3000, 3500, 4000,
    ];
    scale.iter().take_while(|&&t| points >= t).count() as u64
}

#[test]
#[cfg_attr(debug_assertions, ignore = "slow unoptimized: run with --release")]
fn coarse_oracle_agrees_on_line_comparisons() {
    // The engine's lines treat cards declarer cannot yet tell apart alike,
    // so they are the coarse oracle's root front, not the full oracle's:
    // telling East's 7 from the 9 once the 8 between them is gone takes a
    // trick more on West 9QJ in 32 / KT864, a line no class-blind strategy
    // has (the full front has 59 such vectors to the engine's 58)
    let mut fails = Vec::new();
    let mut mixes = 0;
    for (n, s) in random_combos(80, 3, 6, 0x9E37_79B9_7F4A_7C15)
        .into_iter()
        .chain([(holding("32"), holding("KT864"))])
        // Lines beating each other in a cycle at MP: a mix of five is best
        .chain([(holding("Q87"), holding("AT65"))])
    {
        let missing = DECK & !n.to_bits() & !s.to_bits();
        let mut coarse = Coarse {
            missing,
            memo: HashMap::new(),
        };
        let root = coarse.solve(CPos {
            hands: [n.to_bits(), s.to_bits()],
            canon: [0; 2],
            out: [false; 2],
            leader: NORTH,
            trick: [0; 4],
            count: 0,
        });
        let (lanes, front) = &*root;
        let weights: Vec<u64> = lanes.iter().map(|l| vacant_places(missing, l.0)).collect();
        let max = n.len().max(s.len());
        let dist = |v: &Vector| {
            let mut d = vec![0; max + 1];
            for (i, &t) in v.iter().enumerate() {
                d[usize::from(t)] += weights[i];
            }
            d
        };
        // Making only with every trick in the suit, so that undertricks
        // count and no total passes 13: 3NT vulnerable, and 2NT not, where
        // one down against making is 170 points, a step of the scale
        let need = max as u8;
        let imps_in = |contract: &str, vulnerable| Scoring::Imps {
            contract: contract.parse().unwrap(),
            vulnerable,
            need,
        };
        let scorings = [
            Scoring::Matchpoints,
            imps_in("3N", true),
            imps_in("2N", false),
        ];
        for scoring in scorings {
            let gain = |x: u8, y: u8| match scoring {
                Scoring::Matchpoints => u64::from(x > y),
                Scoring::Imps {
                    contract,
                    vulnerable,
                    ..
                } => {
                    let level = contract.bid.level.get();
                    let score = |t: u8| contract.score(6 + level + t - need, vulnerable);
                    imps(score(x) - score(y))
                }
            };
            let mut solver = Solver::new(n, s).unwrap();
            let name = format!("{} {scoring:?}", name(n, s));
            let mut want: Vec<Vec<u64>> = front.iter().map(dist).collect();
            let mut got: Vec<Vec<u64>> = (solver.front().iter())
                .map(|g| {
                    let mut d = vec![0; max + 1];
                    for (t, l) in g.iter().zip(solver.layouts()) {
                        d[usize::from(*t)] += l.weight;
                    }
                    d
                })
                .collect();
            want.sort();
            got.sort();
            if want != got {
                fails.push(format!("{name}: lines {got:?} coarse oracle {want:?}"));
                continue;
            }
            // Match lines by distribution where it tells them apart
            let find = |d: &Vec<u64>| {
                let mut it = front.iter().filter(|v| dist(v) == *d);
                it.next().filter(|_| it.next().is_none())
            };
            let c = solver.compare(scoring).unwrap();
            // Lines found but off the front may share a distribution with
            // a line on it
            let every = solver.front();
            let find = |l: &suit_combination::Line| {
                find(&l.tricks).filter(|_| every.contains(&l.guarantee))
            };
            // Proven: no line on the full front, as checked above, beats
            // the best, or gains more than the slack over the best mix;
            // else some line does
            let over = |g: &Vec<u8>| -> i64 {
                (c.best.iter())
                    .map(|&i| {
                        let l = &c.lines[i].guarantee;
                        let net: i64 = (0..g.len())
                            .map(|r| {
                                let (x, y) = (usize::from(g[r]), usize::from(l[r]));
                                c.weights[r] as i64
                                    * (c.payoff[x][y] as i64 - c.payoff[y][x] as i64)
                            })
                            .sum();
                        c.mix[i] as i64 * net
                    })
                    .sum()
            };
            let beaten = every.iter().any(|g| over(g) > c.slack as i64);
            mixes += usize::from(c.proven && c.best.len() > 1);
            if c.proven == beaten || c.open {
                fails.push(format!(
                    "{name}: proven {} open {}, beaten {beaten}",
                    c.proven, c.open
                ));
            }
            let Some(best) = find(&c.lines[0]) else {
                continue;
            };
            for (a, la) in c.lines.iter().enumerate() {
                let Some(va) = find(la) else {
                    continue;
                };
                let w: u64 = (0..va.len())
                    .map(|i| weights[i] * gain(va[i], best[i]))
                    .sum();
                if c.gain(a, 0) != w {
                    fails.push(format!(
                        "{name} gain {a} 0: engine {} oracle {w}",
                        c.gain(a, 0)
                    ));
                }
            }
        }
    }
    assert!(mixes > 0, "no best mix checked");
    assert!(
        fails.is_empty(),
        "
{}",
        fails.join(
            "
"
        )
    );
}

/// Position as declarer sees it under the engine's compression: a
/// defender's card is seen only as its class (missing cards with no live
/// declarer card between them), and classes merge as declarer's cards go.
/// Standard rules only: either hand leads, the suit is played out.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct CPos {
    hands: [u16; 2],
    /// Per defender (East, West): in each class, as many of its lowest
    /// ranks as cards that defender has played from it
    canon: [u16; 2],
    out: [bool; 2],
    leader: usize,
    /// Declarer's ranks; a defender's card as its class's lowest rank
    trick: [u8; 4],
    count: usize,
}

/// A concrete lane: (West's cards dealt, cards East played, cards West
/// played).  Lanes stay concrete; only declarer's view is coarse.
type Lane = (u16, u16, u16);

/// The lanes declarer cannot tell apart at a position, and the front over them
type Front = (Vec<Lane>, Vec<Vector>);

/// Oracle restricted to strategies that depend on the coarse view only:
/// the engine's model, solved independently of the engine
struct Coarse {
    missing: u16,
    memo: HashMap<CPos, Rc<Front>>,
}

impl Coarse {
    fn live(p: &CPos) -> u16 {
        let mut live = p.hands[0] | p.hands[1];
        for i in 0..p.count {
            let s = (p.leader + i) % 4;
            if s.is_multiple_of(2) && p.trick[s] != 0 {
                live |= 1 << p.trick[s];
            }
        }
        live
    }

    fn classes(&self, live: u16) -> Vec<u16> {
        let mut out = Vec::new();
        let mut cur = 0u16;
        for r in 2..16u8 {
            if r < 15 && self.missing >> r & 1 != 0 {
                cur |= 1 << r;
            } else if cur != 0 && (r == 15 || live >> r & 1 != 0) {
                out.push(cur);
                cur = 0;
            }
        }
        out
    }

    /// `played` (concrete, or an older canonical form) as counts per class
    fn canon(classes: &[u16], played: u16) -> u16 {
        let mut c = 0;
        for &k in classes {
            let mut m = k;
            for _ in 0..(played & k).count_ones() {
                c |= m & m.wrapping_neg();
                m &= m - 1;
            }
        }
        c
    }

    /// Every concrete lane declarer cannot tell from the others at `p`
    fn lanes(&self, p: &CPos) -> Vec<Lane> {
        let cl = self.classes(Self::live(p));
        let mut out = Vec::new();
        for west in subsets(self.missing) {
            let east = self.missing & !west;
            for pe in subsets(east) {
                if Self::canon(&cl, pe) != p.canon[0] || (p.out[0] && pe != east) {
                    continue;
                }
                for pw in subsets(west) {
                    if Self::canon(&cl, pw) == p.canon[1] && !(p.out[1] && pw != west) {
                        out.push((west, pe, pw));
                    }
                }
            }
        }
        out
    }

    fn play(&self, mut p: CPos, seat: usize, rank: u8) -> (CPos, bool) {
        if p.count == 0 {
            p.leader = seat;
        }
        if seat.is_multiple_of(2) {
            p.hands[seat / 2] &= !(1u16 << rank);
        } else if rank == 0 {
            p.out[seat / 2] = true;
        } else {
            let cl = self.classes(Self::live(&p));
            let c = *cl.iter().find(|&&k| k >> rank & 1 != 0).unwrap();
            let free = c & !p.canon[seat / 2];
            p.canon[seat / 2] |= free & free.wrapping_neg();
        }
        p.trick[seat] = rank;
        p.count += 1;
        if p.count < 4 {
            return (p, false);
        }
        let winner = (0..4).max_by_key(|&s| p.trick[s]).unwrap();
        p.leader = NORTH;
        p.trick = [0; 4];
        p.count = 0;
        let cl = self.classes(Self::live(&p));
        p.canon = p.canon.map(|c| Self::canon(&cl, c));
        (p, winner % 2 == 0)
    }

    fn solve(&mut self, p: CPos) -> Rc<Front> {
        if let Some(r) = self.memo.get(&p) {
            return Rc::clone(r);
        }
        let lanes = self.lanes(&p);
        let vecs = if p.count == 0 && p.hands == [0, 0] {
            vec![vec![0; lanes.len()]]
        } else if p.count.is_multiple_of(2) {
            let seats: Vec<usize> = if p.count == 0 {
                [NORTH, SOUTH]
                    .into_iter()
                    .filter(|&s| p.hands[s / 2] != 0)
                    .collect()
            } else {
                vec![(p.leader + p.count) % 4]
            };
            let mut all = Vec::new();
            for s in seats {
                let h = p.hands[s / 2];
                let ranks: Vec<u8> = if h == 0 { vec![0] } else { cards(h).collect() };
                for r in ranks {
                    let (ch, _) = self.play(p, s, r);
                    let sub = self.solve(ch);
                    let idx: HashMap<Lane, usize> =
                        sub.0.iter().enumerate().map(|(i, &l)| (l, i)).collect();
                    let map: Vec<usize> = lanes.iter().map(|l| idx[l]).collect();
                    for y in &sub.1 {
                        all.push(map.iter().map(|&i| y[i]).collect());
                    }
                }
            }
            prune(all)
        } else {
            self.defend(p, &lanes, (p.leader + p.count) % 4)
        };
        let out = Rc::new((lanes, vecs));
        self.memo.insert(p, Rc::clone(&out));
        out
    }

    /// A defender plays a class (or shows out); in each lane any card of
    /// it they hold, the worst for declarer
    fn defend(&mut self, p: CPos, lanes: &[Lane], seat: usize) -> Vec<Vector> {
        let d = seat / 2;
        let cl = self.classes(Self::live(&p));
        let mut branches = Vec::new();
        for c in cl.iter().copied().map(Some).chain([None]) {
            let rank = c.map_or(0, |c| c.trailing_zeros() as u8);
            let mut legal = Vec::new();
            let mut opts: Vec<Vec<Lane>> = Vec::new();
            for (j, l) in lanes.iter().enumerate() {
                let (dealt, played) = if d == 1 {
                    (l.0, l.2)
                } else {
                    (self.missing & !l.0, l.1)
                };
                let held = dealt & !played;
                let xs: Vec<u16> = match c {
                    Some(c) => cards(held & c).map(|r| 1u16 << r).collect(),
                    None if held == 0 => vec![0],
                    None => vec![],
                };
                if xs.is_empty() {
                    continue;
                }
                legal.push(j);
                opts.push(
                    xs.iter()
                        .map(|&x| {
                            if d == 1 {
                                (l.0, l.1, l.2 | x)
                            } else {
                                (l.0, l.1 | x, l.2)
                            }
                        })
                        .collect(),
                );
            }
            if legal.is_empty() {
                continue;
            }
            let (ch, won) = self.play(p, seat, rank);
            let sub = self.solve(ch);
            let idx: HashMap<Lane, usize> =
                sub.0.iter().enumerate().map(|(i, &l)| (l, i)).collect();
            let ys: Vec<Vector> = sub
                .1
                .iter()
                .map(|y| {
                    opts.iter()
                        .map(|o| o.iter().map(|l| y[idx[l]]).min().unwrap() + u8::from(won))
                        .collect()
                })
                .collect();
            branches.push((legal, prune(ys)));
        }
        branches.sort_by_key(|b| b.1.len());
        let mut acc = vec![vec![u8::MAX; lanes.len()]];
        for (legal, branch) in branches {
            let mut next = Vec::new();
            for x in &acc {
                if branch
                    .iter()
                    .any(|y| legal.iter().enumerate().all(|(m, &j)| y[m] >= x[j]))
                {
                    next.push(x.clone());
                    continue;
                }
                for y in &branch {
                    let mut z = x.clone();
                    for (m, &j) in legal.iter().enumerate() {
                        z[j] = z[j].min(y[m]);
                    }
                    next.push(z);
                }
            }
            acc = prune(next);
        }
        acc
    }
}
