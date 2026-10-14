//! Single-dummy suit-combination solver for contract bridge.
//!
//! Declarer (North–South) sees only the cards played; the defenders see
//! everything and, for their actual layout, pick the card that hurts most.
//! [`analyze`] finds the pure declarer strategy that is best for a [`Goal`]:
//! taking at least some number of tricks in the suit, or the most tricks on
//! average.  Layouts are weighted by vacant places: one giving West `w` of
//! the `n` missing cards has weight `C(26 - n, 13 - w)` out of
//! [`TOTAL`]`= C(26, 13)`.
//!
//! The search is exact.  Instead of a greedy per-branch value — which is *not*
//! optimal here, because a falsecarding defender spreads one layout's weight
//! over several branches — each information state carries the Pareto antichain
//! of "tricks per layout" vectors some continuation guarantees, so the
//! recommended line is the true optimum against omniscient defence.
//!
//! Missing cards with no live declarer card between them are interchangeable:
//! declarer never competes one defender's card against the other's, and a
//! strategy that told them apart could be replaced by a symmetric one no
//! worse.  So a layout only records how many of each such class West holds,
//! and classes merge as the declarer cards separating them are played.
//!
//! Assumptions (the usual ones, as in Pavlicek's card-combination tables):
//! unlimited entries to both hands, no ruffs, no inference about the enemy
//! hands, and only tricks won in this suit count.

use contract_bridge::{Holding, Rank, Seat};
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// `C(26, 13)`: the denominator of every probability in this crate
pub const TOTAL: u64 = 10_400_600;

/// Most missing cards supported (layout sets are 256-bit)
pub const MAX_MISSING: usize = 8;

/// Why a problem cannot be analyzed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// North and South hold a card in common
    Overlap,
    /// More than [`MAX_MISSING`] cards missing
    TooManyMissing,
    /// Target exceeds the longer hand (or is zero)
    BadTarget,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Overlap => "North and South share a card",
            Self::TooManyMissing => "more than 8 cards missing",
            Self::BadTarget => "target must be 1..=longer hand",
        })
    }
}

impl std::error::Error for Error {}

/// What declarer is playing for
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", rename_all = "lowercase"))]
pub enum Goal {
    /// At least this many tricks in the suit
    Tricks { target: u8 },
    /// The most tricks on average
    Expected,
}

/// The optimal line as a tree over declarer's observations
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", rename_all = "lowercase"))]
pub enum Plan {
    /// Every trick the line plays for has been won
    Made,
    /// No layout still being played for reaches this branch
    Hopeless,
    /// Declarer plays one of `cards` (all equivalent) from `seat`; empty = discard
    Declarer {
        seat: Seat,
        cards: Holding,
        then: Box<Plan>,
    },
    /// A defender's card decides the continuation; `None` = shows out
    Defender {
        seat: Seat,
        branches: Vec<(Vec<Option<Rank>>, Plan)>,
    },
}

/// One possible first play and the best it can achieve
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Lead {
    pub seat: Seat,
    /// Equivalent cards; leading any of them is the same play
    pub cards: Holding,
    /// Best [`Analysis::score`] after this lead
    pub score: u64,
}

/// Result of [`analyze`]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Analysis {
    pub north: Holding,
    pub south: Holding,
    pub goal: Goal,
    /// For [`Goal::Tricks`], the weight (out of [`TOTAL`]) of the layouts the
    /// optimal line beats; for [`Goal::Expected`], the expected tricks times
    /// [`TOTAL`].  Exact in both cases.
    pub score: u64,
    pub leads: Vec<Lead>,
    pub plan: Plan,
}

impl Analysis {
    /// Success probability ([`Goal::Tricks`]) or expected tricks ([`Goal::Expected`])
    #[must_use]
    pub fn value(&self) -> f64 {
        self.score as f64 / TOTAL as f64
    }
}

/// Best line for `goal` with `north` opposite `south`
pub fn analyze(north: Holding, south: Holding, goal: Goal) -> Result<Analysis, Error> {
    Solver::new(north, south)?.analyze(goal)
}

/// [`analyze`] for every target from one trick up to the longer hand's
/// length, then for [`Goal::Expected`]
pub fn analyze_all(north: Holding, south: Holding) -> Result<Vec<Analysis>, Error> {
    let mut solver = Solver::new(north, south)?;
    (1..=solver.max_tricks())
        .map(|target| Goal::Tricks { target })
        .chain([Goal::Expected])
        .map(|goal| solver.analyze(goal))
        .collect()
}

/// Set of layouts by index
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
struct Set([u64; 4]);

impl Set {
    const EMPTY: Self = Self([0; 4]);

    fn insert(&mut self, i: usize) {
        self.0[i >> 6] |= 1 << (i & 63);
    }

    fn is_empty(self) -> bool {
        self.0 == [0; 4]
    }

    fn iter(self) -> impl Iterator<Item = usize> {
        (0..4).flat_map(move |k| {
            let mut w = self.0[k];
            std::iter::from_fn(move || {
                (w != 0).then(|| {
                    let b = w.trailing_zeros() as usize;
                    w &= w - 1;
                    k << 6 | b
                })
            })
        })
    }
}

impl std::ops::BitAnd for Set {
    type Output = Self;
    fn bitand(self, o: Self) -> Self {
        Self(std::array::from_fn(|k| self.0[k] & o.0[k]))
    }
}

impl std::ops::Sub for Set {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self(std::array::from_fn(|k| self.0[k] & !o.0[k]))
    }
}

/// Tricks declarer is guaranteed from here, per layout (0 outside the
/// layouts consistent with the state); eight layouts per word
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Tricks([u64; 1 << MAX_MISSING >> 3]);

impl Tricks {
    const ZERO: Self = Self([0; 1 << MAX_MISSING >> 3]);
    /// High bit of every byte lane
    const H: u64 = 0x8080_8080_8080_8080;

    fn get(&self, i: usize) -> u8 {
        (self.0[i >> 3] >> ((i & 7) * 8)) as u8
    }

    fn set(&mut self, i: usize, t: u8) {
        let shift = (i & 7) * 8;
        self.0[i >> 3] = self.0[i >> 3] & !(0xFF << shift) | u64::from(t) << shift;
    }

    /// Every lane of `self` is at least the lane of `need` (lanes stay below 128)
    fn covers(&self, need: &Self, len: usize) -> bool {
        let words = len.div_ceil(8);
        self.0[..words]
            .iter()
            .zip(&need.0[..words])
            .all(|(&v, &r)| ((v | Self::H) - r) & Self::H == Self::H)
    }

    fn sum(&self, len: usize) -> u32 {
        self.0[..len.div_ceil(8)]
            .iter()
            .flat_map(|w| w.to_ne_bytes())
            .map(u32::from)
            .sum()
    }
}

/// Drop duplicates and every vector dominated by another; what remains is a
/// Pareto antichain
fn maximal(mut vs: Vec<Tricks>, len: usize) -> Vec<Tricks> {
    vs.sort_unstable_by_key(|v| (std::cmp::Reverse(v.sum(len)), *v));
    vs.dedup();
    let mut out: Vec<Tricks> = Vec::new();
    for v in vs {
        if !out.iter().any(|o| o.covers(&v, len)) {
            out.push(v);
        }
    }
    out
}

const NORTH: u8 = 0;
const SOUTH: u8 = 2;

fn seat(i: u8) -> Seat {
    [Seat::North, Seat::East, Seat::South, Seat::West][i as usize]
}

/// Everything declarer knows, which is all that a strategy may depend on
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct State {
    /// Remaining cards of North, South
    ns: [u16; 2],
    /// Cards already played by East, West, as a count per class of the
    /// state's [`Structure`]
    shown: [[u8; MAX_MISSING]; 2],
    /// East, West have shown out
    void: [bool; 2],
    /// Seat that led the current trick (NORTH when between tricks)
    leader: u8,
    /// Ranks played so far in the current trick by position; 0 = discarded,
    /// a defender's card is its class's lowest rank
    trick: [u8; 4],
    played: u8,
}

impl State {
    /// Declarer cards still able to separate missing cards: in hand or on the table
    fn live(&self) -> u16 {
        let mut live = self.ns[0] | self.ns[1];
        for p in 0..usize::from(self.played) {
            if (self.leader + p as u8).is_multiple_of(2) && self.trick[p] != 0 {
                live |= 1 << self.trick[p];
            }
        }
        live
    }
}

fn ranks(mask: u16) -> impl Iterator<Item = u8> {
    (2..15).filter(move |r| mask >> r & 1 != 0)
}

fn lowest(mask: u16) -> u8 {
    mask.trailing_zeros() as u8
}

/// Missing cards with no live declarer card between them
struct Class {
    ranks: u16,
    size: u8,
    /// Multiplier of West's count of this class in a layout index
    stride: usize,
}

/// The layout space for one set of live declarer cards
struct Structure {
    classes: Vec<Class>,
    /// Number of layouts
    len: usize,
    /// Weight of each layout (number of deals, out of [`TOTAL`])
    weight: Vec<u64>,
    /// `ge[d][c][k]`: layouts in which defender `d` (0 East, 1 West) holds at
    /// least `k` cards of class `c`; `k` runs one past the class size
    ge: [Vec<Vec<Set>>; 2],
    all: Set,
}

fn binom(n: u64, k: u64) -> u64 {
    (0..k).fold(1, |acc, i| acc * (n - i) / (i + 1))
}

impl Structure {
    fn new(missing: u16, live: u16) -> Self {
        let n = missing.count_ones() as usize;
        let mut classes: Vec<Class> = Vec::new();
        let mut len = 1;
        let mut cur = 0u16;
        for r in 2..16u8 {
            if r < 15 && missing >> r & 1 != 0 {
                cur |= 1 << r;
            } else if cur != 0 && (r == 15 || live >> r & 1 != 0) {
                let size = cur.count_ones() as u8;
                classes.push(Class {
                    ranks: cur,
                    size,
                    stride: len,
                });
                len *= usize::from(size) + 1;
                cur = 0;
            }
        }
        let count = |i: usize, c: &Class| i / c.stride % (usize::from(c.size) + 1);
        let weight = (0..len)
            .map(|i| {
                let west: usize = classes.iter().map(|c| count(i, c)).sum();
                classes
                    .iter()
                    .map(|c| binom(c.size.into(), count(i, c) as u64))
                    .product::<u64>()
                    * binom(26 - n as u64, 13 - west as u64)
            })
            .collect();
        let ge = std::array::from_fn(|d| {
            classes
                .iter()
                .map(|c| {
                    (0..=usize::from(c.size) + 1)
                        .map(|k| {
                            let mut s = Set::EMPTY;
                            for i in 0..len {
                                let west = count(i, c);
                                let held = if d == 1 {
                                    west
                                } else {
                                    usize::from(c.size) - west
                                };
                                if held >= k {
                                    s.insert(i);
                                }
                            }
                            s
                        })
                        .collect()
                })
                .collect()
        });
        let mut all = Set::EMPTY;
        for i in 0..len {
            all.insert(i);
        }
        Self {
            classes,
            len,
            weight,
            ge,
            all,
        }
    }

    /// Index of the class holding `rank`
    fn class_of(&self, rank: u8) -> usize {
        self.classes
            .iter()
            .position(|c| c.ranks >> rank & 1 != 0)
            .unwrap()
    }

    /// For each layout of `self`, the layout of the coarser `child` it becomes
    fn map_to(&self, child: &Self) -> Vec<usize> {
        (0..self.len)
            .map(|i| {
                self.classes
                    .iter()
                    .map(|c| {
                        child.classes[child.class_of(lowest(c.ranks))].stride
                            * (i / c.stride % (usize::from(c.size) + 1))
                    })
                    .sum()
            })
            .collect()
    }
}

struct Solver {
    north: Holding,
    south: Holding,
    missing: u16,
    /// Layout spaces by live declarer cards
    structures: HashMap<u16, Rc<Structure>>,
    memo: HashMap<State, Vec<Tricks>>,
}

impl Solver {
    fn new(north: Holding, south: Holding) -> Result<Self, Error> {
        if !(north & south).is_empty() {
            return Err(Error::Overlap);
        }
        let missing = (Holding::ALL - north - south).to_bits();
        if missing.count_ones() as usize > MAX_MISSING {
            return Err(Error::TooManyMissing);
        }
        Ok(Self {
            north,
            south,
            missing,
            structures: HashMap::new(),
            memo: HashMap::new(),
        })
    }

    fn structure(&mut self, live: u16) -> Rc<Structure> {
        Rc::clone(
            self.structures
                .entry(live)
                .or_insert_with(|| Rc::new(Structure::new(self.missing, live))),
        )
    }

    fn max_tricks(&self) -> u8 {
        self.north.len().max(self.south.len()) as u8
    }

    fn score(root: &Structure, goal: Goal, v: &Tricks) -> u64 {
        (0..root.len)
            .map(|i| match goal {
                Goal::Tricks { target } => u64::from(v.get(i) >= target) * root.weight[i],
                Goal::Expected => u64::from(v.get(i)) * root.weight[i],
            })
            .sum()
    }

    fn analyze(&mut self, goal: Goal) -> Result<Analysis, Error> {
        if let Goal::Tricks { target } = goal
            && (target == 0 || target > self.max_tricks())
        {
            return Err(Error::BadTarget);
        }
        let root = State {
            ns: [self.north.to_bits(), self.south.to_bits()],
            shown: [[0; MAX_MISSING]; 2],
            void: [false; 2],
            leader: NORTH,
            trick: [0; 4],
            played: 0,
        };
        let structure = self.structure(root.live());
        let moves = self.declarer_moves(&root, &structure);
        let mut leads = Vec::new();
        let mut best: Option<(u64, Tricks, usize)> = None;
        for (i, (st, cards)) in moves.iter().enumerate() {
            let (score, v) = self
                .feasible(*st)
                .into_iter()
                .map(|v| (Self::score(&structure, goal, &v), v))
                .max_by_key(|&(s, _)| s)
                .unwrap_or((0, Tricks::ZERO));
            leads.push(Lead {
                seat: seat(st.leader),
                cards: Holding::from_bits_truncate(*cards),
                score,
            });
            if best.is_none_or(|(s, _, _)| score > s) {
                best = Some((score, v, i));
            }
        }
        let (score, mut req, i) = best.ok_or(Error::BadTarget)?;
        // Tricks per layout the line must still win to realize its vector.
        if let Goal::Tricks { target } = goal {
            for i in 0..structure.len {
                req.set(i, if req.get(i) >= target { target } else { 0 });
            }
        }
        let plan = Plan::Declarer {
            seat: leads[i].seat,
            cards: leads[i].cards,
            then: Box::new(self.plan(moves[i].0, req)),
        };
        Ok(Analysis {
            north: self.north,
            south: self.south,
            goal,
            score,
            leads,
            plan,
        })
    }

    /// Layouts consistent with what declarer has seen
    fn consistent(st: &State, s: &Structure) -> Set {
        let mut c = s.all;
        for d in 0..2 {
            for (k, ge) in s.ge[d].iter().enumerate() {
                let shown = usize::from(st.shown[d][k]);
                c = c & ge[shown];
                if st.void[d] {
                    c = c - ge[shown + 1];
                }
            }
        }
        c
    }

    /// Cards of `hand` grouped into runs with no other live card in between.
    /// Cards on the table in the current trick are live: a jack and a five
    /// are not interchangeable over a defender's eight.
    fn classes(st: &State, s: &Structure, hand: u16) -> Vec<u16> {
        let missing_left = s
            .classes
            .iter()
            .enumerate()
            .filter(|&(k, c)| st.shown[0][k] + st.shown[1][k] < c.size)
            .fold(0, |acc, (_, c)| acc | 1 << lowest(c.ranks));
        let deck = st.ns[0]
            | st.ns[1]
            | missing_left
            | st.trick
                .iter()
                .fold(0, |acc, &r| acc | u16::from(r != 0) << r);
        let mut classes = Vec::new();
        let mut cur = 0u16;
        for r in ranks(deck) {
            if hand >> r & 1 != 0 {
                cur |= 1 << r;
            } else if cur != 0 {
                classes.push(cur);
                cur = 0;
            }
        }
        if cur != 0 {
            classes.push(cur);
        }
        classes
    }

    /// Record `rank` (0 = discard) at the current position.  A completed
    /// trick is resolved (the flag says whether declarer won it) and the
    /// state re-expressed in the coarser structure its dead cards allow.
    fn advance(&mut self, mut st: State, s: &Structure, rank: u8) -> (State, bool) {
        st.trick[st.played as usize] = rank;
        st.played += 1;
        if st.played < 4 {
            return (st, false);
        }
        let (pos, _) = st
            .trick
            .iter()
            .enumerate()
            .max_by_key(|&(_, &r)| r)
            .unwrap();
        let won = (st.leader + pos as u8).is_multiple_of(2);
        st.trick = [0; 4];
        st.played = 0;
        st.leader = NORTH;
        let coarse = self.structure(st.live());
        if coarse.classes.len() != s.classes.len() {
            let shown = st.shown;
            st.shown = [[0; MAX_MISSING]; 2];
            for (k, c) in s.classes.iter().enumerate() {
                let to = coarse.class_of(lowest(c.ranks));
                for (d, shown) in shown.iter().enumerate() {
                    st.shown[d][to] += shown[k];
                }
            }
        }
        (st, won)
    }

    /// Declarer's options now: (resulting state, equivalence class played).
    /// Between tricks that includes the choice of hand; at third hand a void
    /// hand has the single option "discard" (class 0).  Declarer never plays
    /// fourth, so these moves never complete a trick or change the structure.
    fn declarer_moves(&mut self, st: &State, s: &Structure) -> Vec<(State, u16)> {
        let mut out = Vec::new();
        let seats: &[u8] = if st.played == 0 {
            &[NORTH, SOUTH]
        } else {
            &[(st.leader + st.played) % 4]
        };
        for &seat in seats {
            let hand = st.ns[seat as usize / 2];
            if hand == 0 {
                if st.played != 0 {
                    out.push((self.advance(*st, s, 0).0, 0));
                }
                continue;
            }
            for class in Self::classes(st, s, hand) {
                let mut ch = *st;
                ch.leader = if st.played == 0 { seat } else { st.leader };
                ch.ns[seat as usize / 2] &= !(1 << lowest(class));
                out.push((self.advance(ch, s, lowest(class)).0, class));
            }
        }
        out
    }

    /// A defender's options now: (class played, layouts where it is legal,
    /// resulting state, declarer won the trick); `None` = shows out
    fn defender_moves(
        &mut self,
        st: &State,
        s: &Structure,
        c: Set,
    ) -> Vec<(Option<usize>, Set, State, bool)> {
        let d = ((st.leader + st.played) % 4 / 2) as usize; // 0 = East, 1 = West
        let mut out = Vec::new();
        let mut void = c;
        for (k, class) in s.classes.iter().enumerate() {
            let shown = usize::from(st.shown[d][k]);
            let g = c & s.ge[d][k][shown + 1];
            void = void - g;
            if !g.is_empty() {
                let mut ch = *st;
                ch.shown[d][k] += 1;
                let (ch, won) = self.advance(ch, s, lowest(class.ranks));
                out.push((Some(k), g, ch, won));
            }
        }
        if !void.is_empty() {
            let mut ch = *st;
            ch.void[d] = true;
            let (ch, won) = self.advance(ch, s, 0);
            out.push((None, void, ch, won));
        }
        out
    }

    fn is_declarer_turn(st: &State) -> bool {
        st.played == 0 || (st.leader + st.played).is_multiple_of(2)
    }

    /// Pareto-maximal "tricks per layout" vectors (over the layouts
    /// consistent with `st`) that some declarer continuation guarantees
    fn feasible(&mut self, st: State) -> Vec<Tricks> {
        if let Some(v) = self.memo.get(&st) {
            return v.clone();
        }
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        let out = if st.played == 0 && st.ns == [0; 2] {
            vec![Tricks::ZERO]
        } else if Self::is_declarer_turn(&st) {
            let moves = self.declarer_moves(&st, &s);
            maximal(
                moves
                    .into_iter()
                    .flat_map(|(ch, _)| self.feasible(ch))
                    .collect(),
                s.len,
            )
        } else {
            // The defender routes each layout to its worst legal branch, so a
            // continuation guarantees per layout the minimum over the
            // branches where that layout may legally play.  A completed trick
            // may coarsen the structure; `map` lifts the child's vectors.
            let mut start = Tricks::ZERO;
            for i in c.iter() {
                start.set(i, 127);
            }
            let mut acc = vec![start];
            for (_, g, ch, won) in self.defender_moves(&st, &s, c) {
                let map = s.map_to(&self.structure(ch.live()));
                let bonus = u8::from(won);
                // Only the branch's legal layouts matter, so project onto
                // them first: that collapses many continuations into one.
                let sub = maximal(
                    self.feasible(ch)
                        .iter()
                        .map(|y| {
                            let mut p = Tricks::ZERO;
                            for i in g.iter() {
                                p.set(i, y.get(map[i]) + bonus);
                            }
                            p
                        })
                        .collect(),
                    s.len,
                );
                let mut next = Vec::with_capacity(acc.len() * sub.len());
                for x in &acc {
                    for y in &sub {
                        let mut z = *x;
                        for i in g.iter() {
                            z.set(i, z.get(i).min(y.get(i)));
                        }
                        next.push(z);
                    }
                }
                acc = maximal(next, s.len);
            }
            acc
        };
        self.memo.insert(st, out.clone());
        out
    }

    /// A continuation from `st` that wins at least `req` tricks per layout
    /// (requires `req` to be covered by some feasible vector of `st`)
    fn plan(&mut self, st: State, req: Tricks) -> Plan {
        let s = self.structure(st.live());
        if (0..s.len).all(|i| req.get(i) == 0) {
            return Plan::Made;
        }
        let c = Self::consistent(&st, &s);
        if Self::is_declarer_turn(&st) {
            for (ch, cards) in self.declarer_moves(&st, &s) {
                if self.feasible(ch).iter().any(|v| v.covers(&req, s.len)) {
                    let seat_index = if st.played == 0 {
                        ch.leader
                    } else {
                        (st.leader + st.played) % 4
                    };
                    return Plan::Declarer {
                        seat: seat(seat_index),
                        cards: Holding::from_bits_truncate(cards),
                        then: Box::new(self.plan(ch, req)),
                    };
                }
            }
            unreachable!("req is feasible, so some move keeps it feasible");
        }
        let seat_index = (st.leader + st.played) % 4;
        let mut branches: Vec<(Vec<Option<Rank>>, Plan)> = Vec::new();
        for (class, g, ch, won) in self.defender_moves(&st, &s, c) {
            let map = s.map_to(&self.structure(ch.live()));
            let mut sub_req = Tricks::ZERO;
            let mut wanted = false;
            for i in g.iter() {
                wanted |= req.get(i) > 0;
                let r = req.get(i).saturating_sub(u8::from(won));
                sub_req.set(map[i], sub_req.get(map[i]).max(r));
            }
            let sub = if wanted {
                self.plan(ch, sub_req)
            } else {
                Plan::Hopeless
            };
            let labels: Vec<Option<Rank>> = match class {
                Some(k) => ranks(s.classes[k].ranks)
                    .map(|r| Some(Rank::new(r)))
                    .collect(),
                None => vec![None],
            };
            match branches.iter_mut().find(|(_, p)| *p == sub) {
                Some((existing, _)) => existing.extend(labels),
                None => branches.push((labels, sub)),
            }
        }
        Plan::Defender {
            seat: seat(seat_index),
            branches,
        }
    }
}

impl Plan {
    fn fmt_indented(&self, f: &mut fmt::Formatter<'_>, depth: usize) -> fmt::Result {
        let pad = "  ".repeat(depth);
        match self {
            Self::Made => writeln!(f, "{pad}made"),
            Self::Hopeless => writeln!(f, "{pad}(hopeless)"),
            Self::Declarer { seat, cards, then } => {
                if cards.is_empty() {
                    writeln!(f, "{pad}{seat:?} discards")?;
                } else {
                    writeln!(f, "{pad}{seat:?} plays {cards}")?;
                }
                then.fmt_indented(f, depth)
            }
            Self::Defender { seat, branches } => {
                for (labels, plan) in branches {
                    let labels: Vec<String> = labels
                        .iter()
                        .map(|l| l.map_or("void".to_string(), |r| r.to_string()))
                        .collect();
                    writeln!(f, "{pad}{seat:?} {}:", labels.join("/"))?;
                    plan.fmt_indented(f, depth + 1)?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_indented(f, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(north: &str, south: &str, goal: Goal) -> u64 {
        analyze(north.parse().unwrap(), south.parse().unwrap(), goal)
            .unwrap()
            .score
    }

    fn tricks(north: &str, south: &str, target: u8) -> u64 {
        score(north, south, Goal::Tricks { target })
    }

    #[test]
    fn pavlicek_missing_the_jack() {
        // 4a00 #1: cash A or Q first; every layout can be picked up.
        assert_eq!(tricks("AQ954", "KT32", 5), TOTAL);
        // 4a00 #2: cash the K first; only East holding J876 (4-0) beats it.
        assert_eq!(tricks("AQ954", "K832", 5), TOTAL - binom(22, 13));
    }

    #[test]
    fn splits() {
        // AKQxx opposite xx: five tricks need 3-3, four tricks need 4-2 or better.
        let three_three = 20 * binom(20, 10);
        let four_two = 30 * binom(20, 11);
        let five_one = 12 * binom(20, 12);
        let six_zero = 2 * binom(20, 13);
        assert_eq!(tricks("AKQ32", "54", 5), three_three);
        assert_eq!(tricks("AKQ32", "54", 4), three_three + four_two);
        assert_eq!(
            score("AKQ32", "54", Goal::Expected),
            5 * three_three + 4 * four_two + 3 * (five_one + six_zero)
        );
    }

    #[test]
    fn stiff_king_offside() {
        // AQx opposite xx (8 missing): cash the A first, then lead toward the Q.
        // Beats K onside plus a singleton K offside — better than a plain finesse.
        let two = TOTAL / 2 + binom(18, 6);
        assert_eq!(tricks("AQ3", "42", 2), two);
        assert_eq!(score("AQ3", "42", Goal::Expected), 2 * two + (TOTAL - two));
    }

    #[test]
    fn reading_group_problem_one() {
        // AJ52 opposite K76, three tricks: 3-3, West long 4-2, 2-4 with Q West,
        // 5-1 West long, Q singleton West, 6-0 West.  Derived by hand.
        let w = |west: u64| binom(20, 13 - west);
        let expected = 20 * w(3) + 15 * w(4) + 5 * w(2) + 6 * w(5) + w(1) + w(6);
        assert_eq!(tricks("AJ52", "K76", 3), expected);
    }

    #[test]
    fn errors() {
        let h = |s: &str| s.parse::<Holding>().unwrap();
        let t = |target| Goal::Tricks { target };
        assert_eq!(analyze(h("AK"), h("A2"), t(1)).unwrap_err(), Error::Overlap);
        assert_eq!(
            analyze(h("AK"), h("32"), t(1)).unwrap_err(),
            Error::TooManyMissing
        );
        assert_eq!(
            analyze(h("AKQ"), h("32"), t(4)).unwrap_err(),
            Error::BadTarget
        );
        assert_eq!(analyze_all(h("AKQ"), h("32")).unwrap().len(), 4);
    }
}
