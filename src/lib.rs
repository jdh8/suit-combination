//! Single-dummy suit-combination solver for contract bridge.
//!
//! Declarer (North–South) sees only the cards played; the defenders see
//! everything and, for their actual layout, pick the card that hurts most.
//! [`analyze`] finds the pure declarer strategy that is best for a [`Goal`]:
//! taking at least some number of tricks in the suit, or the most tricks on
//! average.  Layouts are weighted by vacant places: one giving West `w` of
//! the `n` missing cards has weight `C(26 - n, 13 - w)` out of
//! <code>[TOTAL] = C(26, 13)</code>.
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
    /// West was dealt a card North or South holds
    BadWest,
    /// The card is not one declarer may play now
    NotPlayable,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Overlap => "North and South share a card",
            Self::TooManyMissing => "more than 8 cards missing",
            Self::BadTarget => "target must be 1..=longer hand",
            Self::BadWest => "West holds a card of North or South",
            Self::NotPlayable => "that card cannot be played now",
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

/// One card played in a [`Game`]; `None` = a discard or showing out
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Play {
    pub seat: Seat,
    pub card: Option<Rank>,
}

/// Best line for `goal` with `north` opposite `south`
pub fn analyze(north: Holding, south: Holding, goal: Goal) -> Result<Analysis, Error> {
    Solver::new(north, south)?.analyze(goal)
}

/// [`analyze`] for every target from one trick up to the longer hand's
/// length, then for [`Goal::Expected`]
pub fn analyze_all(north: Holding, south: Holding) -> Result<Vec<Analysis>, Error> {
    Solver::new(north, south)?.analyze_all()
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

    /// Lane-wise `min(self, o)` on the lanes of `mask`, `self` elsewhere
    fn min_on(&self, o: &Self, mask: &Self, words: usize) -> Self {
        let mut z = *self;
        for k in 0..words {
            let (a, b) = (self.0[k], o.0[k]);
            let ge = (((a | Self::H) - b) & Self::H) >> 7; // 1 in lanes with a >= b
            let m = (ge * 0xFF) & mask.0[k];
            z.0[k] = (a & !m) | (b & m);
        }
        z
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
fn maximal(vs: Vec<Tricks>, len: usize) -> Vec<Tricks> {
    let words = len.div_ceil(8);
    // Sort an index by sum (descending, then lexicographic for a stable
    // choice of line), not the vectors themselves, and dedup by hashing: a
    // vector can only be dominated by one of larger or equal sum, so the
    // scan below needs only what came before.
    let mut seen: std::collections::HashSet<&[u64]> =
        std::collections::HashSet::with_capacity(vs.len());
    let mut order: Vec<(u32, u32)> = vs
        .iter()
        .enumerate()
        .filter(|(_, v)| seen.insert(&v.0[..words]))
        .map(|(i, v)| (v.sum(len), i as u32))
        .collect();
    order.sort_unstable_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| vs[a.1 as usize].0[..words].cmp(&vs[b.1 as usize].0[..words]))
    });
    let mut out: Vec<Tricks> = Vec::new();
    // The vector that dominated the last candidate usually dominates the next.
    let mut last = 0;
    for &(_, i) in &order {
        let v = &vs[i as usize];
        if out.get(last).is_some_and(|o| o.covers(v, len)) {
            continue;
        }
        match out.iter().position(|o| o.covers(v, len)) {
            Some(j) => last = j,
            None => out.push(*v),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// The search for one combination, memoized across goals and [`Game`]s
pub struct Solver {
    north: Holding,
    south: Holding,
    missing: u16,
    /// Layout spaces by live declarer cards
    structures: HashMap<u16, Rc<Structure>>,
    /// Antichains of tricks per layout, for [`Goal::Expected`]
    memo: HashMap<State, Vec<Tricks>>,
    /// Antichains of 0/1 per layout by tricks still needed, for trick targets
    memo_t: HashMap<(State, u8), Vec<Tricks>>,
}

impl Solver {
    pub fn new(north: Holding, south: Holding) -> Result<Self, Error> {
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
            memo_t: HashMap::new(),
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

    fn check(&self, goal: Goal) -> Result<(), Error> {
        match goal {
            Goal::Tricks { target } if target == 0 || target > self.max_tricks() => {
                Err(Error::BadTarget)
            }
            _ => Ok(()),
        }
    }

    fn root(&self) -> State {
        State {
            ns: [self.north.to_bits(), self.south.to_bits()],
            shown: [[0; MAX_MISSING]; 2],
            void: [false; 2],
            leader: NORTH,
            trick: [0; 4],
            played: 0,
        }
    }

    /// Goal score of guaranteeing `v` more tricks after `won`, over the
    /// layouts `c`
    fn score(s: &Structure, c: Set, goal: Goal, won: u8, v: &Tricks) -> u64 {
        c.iter()
            .map(|i| match goal {
                Goal::Tricks { target } => u64::from(v.get(i) + won >= target) * s.weight[i],
                Goal::Expected => u64::from(v.get(i) + won) * s.weight[i],
            })
            .sum()
    }

    /// Some continuation from `st` wins at least `req` tricks per layout
    fn guarantees(&mut self, st: State, req: &Tricks, len: usize, goal: Goal) -> bool {
        match goal {
            Goal::Expected => self.feasible(st).iter().any(|v| v.covers(req, len)),
            // Every nonzero lane of `req` holds the same count on this path
            Goal::Tricks { .. } => {
                let need = (0..len).map(|i| req.get(i)).max().unwrap_or(0);
                let mut req01 = Tricks::ZERO;
                for i in 0..len {
                    req01.set(i, u8::from(req.get(i) > 0));
                }
                self.feasible_t(st, need)
                    .iter()
                    .any(|v| v.covers(&req01, len))
            }
        }
    }

    /// Best [`score`](Self::score) some continuation from `st` reaches,
    /// with the vector that reaches it: tricks per layout for
    /// [`Goal::Expected`], 0/1 per layout for a trick target
    fn value(&mut self, st: State, goal: Goal, won: u8) -> (u64, Tricks) {
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        let vs = match goal {
            Goal::Tricks { target } => self.feasible_t(st, target.saturating_sub(won)),
            Goal::Expected => self.feasible(st),
        };
        vs.into_iter()
            .map(|v| {
                let score = match goal {
                    Goal::Tricks { .. } => c
                        .iter()
                        .filter(|&i| v.get(i) != 0)
                        .map(|i| s.weight[i])
                        .sum(),
                    Goal::Expected => Self::score(&s, c, goal, won, &v),
                };
                (score, v)
            })
            .max_by_key(|&(x, _)| x)
            .unwrap_or((0, Tricks::ZERO))
    }

    /// [`Analysis`] for every target from one trick up to the longer hand's
    /// length, then for [`Goal::Expected`]
    pub fn analyze_all(&mut self) -> Result<Vec<Analysis>, Error> {
        (1..=self.max_tricks())
            .map(|target| Goal::Tricks { target })
            .chain([Goal::Expected])
            .map(|goal| self.analyze(goal))
            .collect()
    }

    pub fn analyze(&mut self, goal: Goal) -> Result<Analysis, Error> {
        self.check(goal)?;
        let root = self.root();
        let structure = self.structure(root.live());
        let moves = self.declarer_moves(&root, &structure);
        let mut leads = Vec::new();
        let mut best: Option<(u64, Tricks, usize)> = None;
        for (i, &(st, at, cards)) in moves.iter().enumerate() {
            let (score, v) = self.value(st, goal, 0);
            leads.push(Lead {
                seat: seat(at),
                cards: Holding::from_bits_truncate(cards),
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
                req.set(i, req.get(i) * target);
            }
        }
        let plan = Plan::Declarer {
            seat: leads[i].seat,
            cards: leads[i].cards,
            then: Box::new(self.plan(moves[i].0, req, goal)),
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

    /// Declarer's seat `at` plays `rank` (0 = discards) now.  Declarer never
    /// plays fourth, so this never completes a trick or changes the structure.
    fn declarer_play(&mut self, st: &State, s: &Structure, at: u8, rank: u8) -> State {
        let mut ch = *st;
        if st.played == 0 {
            ch.leader = at;
        }
        ch.ns[at as usize / 2] &= !(1 << rank);
        self.advance(ch, s, rank).0
    }

    /// Declarer's options now: (resulting state, seat, equivalence class
    /// played).  Between tricks that includes the choice of hand; at third
    /// hand a void hand has the single option "discard" (class 0).
    fn declarer_moves(&mut self, st: &State, s: &Structure) -> Vec<(State, u8, u16)> {
        let mut out = Vec::new();
        let seats: &[u8] = if st.played == 0 {
            &[NORTH, SOUTH]
        } else {
            &[(st.leader + st.played) % 4]
        };
        for &at in seats {
            let hand = st.ns[at as usize / 2];
            if hand == 0 {
                if st.played != 0 {
                    out.push((self.declarer_play(st, s, at, 0), at, 0));
                }
                continue;
            }
            for class in Self::classes(st, s, hand) {
                out.push((self.declarer_play(st, s, at, lowest(class)), at, class));
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
                    .flat_map(|(ch, _, _)| self.feasible(ch))
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
                let mut mask = Tricks::ZERO;
                for i in g.iter() {
                    mask.set(i, 0xFF);
                }
                let words = s.len.div_ceil(8);
                let mut next = Vec::new();
                for x in &acc {
                    // ponytail: pre-reduce each row; the final filter sees far fewer
                    next.extend(maximal(
                        sub.iter().map(|y| x.min_on(y, &mask, words)).collect(),
                        s.len,
                    ));
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
    fn plan(&mut self, st: State, req: Tricks, goal: Goal) -> Plan {
        let s = self.structure(st.live());
        if (0..s.len).all(|i| req.get(i) == 0) {
            return Plan::Made;
        }
        let c = Self::consistent(&st, &s);
        if Self::is_declarer_turn(&st) {
            for (ch, at, cards) in self.declarer_moves(&st, &s) {
                if self.guarantees(ch, &req, s.len, goal) {
                    return Plan::Declarer {
                        seat: seat(at),
                        cards: Holding::from_bits_truncate(cards),
                        then: Box::new(self.plan(ch, req, goal)),
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
                self.plan(ch, sub_req, goal)
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

/// The suit played out trick by trick: declarer picks a card at each turn
/// and the defenders, who hold the actual layout, answer with the card that
/// hurts most.
#[derive(Debug, Clone)]
pub struct Game {
    goal: Goal,
    st: State,
    /// West's cards as dealt
    west: u16,
    /// Cards still held by East, West
    hands: [u16; 2],
    won: u8,
}

impl Game {
    /// Deal `west` the given cards (East gets the rest of the missing ones)
    /// and play for `goal`
    pub fn new(solver: &Solver, west: Holding, goal: Goal) -> Result<Self, Error> {
        solver.check(goal)?;
        let west = west.to_bits();
        if west & !solver.missing != 0 {
            return Err(Error::BadWest);
        }
        Ok(Self {
            goal,
            st: solver.root(),
            west,
            hands: [solver.missing & !west, west],
            won: 0,
        })
    }

    pub fn north(&self) -> Holding {
        Holding::from_bits_truncate(self.st.ns[0])
    }

    pub fn south(&self) -> Holding {
        Holding::from_bits_truncate(self.st.ns[1])
    }

    pub fn tricks_won(&self) -> u8 {
        self.won
    }

    pub fn is_over(&self) -> bool {
        self.st.played == 0 && self.st.ns == [0; 2]
    }

    /// Weight (out of [`TOTAL`]) of the layouts consistent with what declarer
    /// has seen: the denominator of [`Lead::score`] from here
    pub fn weight(&self, solver: &mut Solver) -> u64 {
        let s = solver.structure(self.st.live());
        Solver::consistent(&self.st, &s)
            .iter()
            .map(|i| s.weight[i])
            .sum()
    }

    /// Declarer's options now, each scored by the best the goal can still
    /// reach after it; empty once the suit is played out
    pub fn moves(&self, solver: &mut Solver) -> Vec<Lead> {
        let s = solver.structure(self.st.live());
        solver
            .declarer_moves(&self.st, &s)
            .into_iter()
            .map(|(ch, at, cards)| Lead {
                seat: seat(at),
                cards: Holding::from_bits_truncate(cards),
                score: solver.value(ch, self.goal, self.won).0,
            })
            .collect()
    }

    /// Declarer plays `rank`; the defenders answer, and forced declarer cards
    /// follow, until declarer has a choice again or the suit is played out.
    /// Returns every card played, in order.
    pub fn play(&mut self, solver: &mut Solver, rank: Rank) -> Result<Vec<Play>, Error> {
        let s = solver.structure(self.st.live());
        let r = rank.get();
        let (_, at, _) = solver
            .declarer_moves(&self.st, &s)
            .into_iter()
            .find(|&(_, _, class)| class >> r & 1 != 0)
            .ok_or(Error::NotPlayable)?;
        self.st = solver.declarer_play(&self.st, &s, at, r);
        let mut plays = vec![Play {
            seat: seat(at),
            card: Some(rank),
        }];
        while !self.is_over() {
            let s = solver.structure(self.st.live());
            if Solver::is_declarer_turn(&self.st) {
                let moves = solver.declarer_moves(&self.st, &s);
                let [(ch, at, class)] = moves[..] else { break };
                self.st = ch;
                plays.push(Play {
                    seat: seat(at),
                    card: Rank::try_new(lowest(class)).ok(),
                });
            } else {
                plays.push(self.defend(solver, &s));
            }
        }
        Ok(plays)
    }

    /// The defender to play picks, among the cards they hold, the one after
    /// which declarer's goal fares worst on the actual layout, breaking ties
    /// by declarer's best score over every layout still consistent — the
    /// most deceptive card.
    // ponytail: that tie-break is a heuristic; a mixed (randomized) defence
    // is the game-theoretic answer if a falsecard is ever found to be
    // predictable from it.
    fn defend(&mut self, solver: &mut Solver, s: &Structure) -> Play {
        let st = self.st;
        let at = (st.leader + st.played) % 4;
        let d = usize::from(at / 2);
        let hand = self.hands[d];
        let c = Solver::consistent(&st, s);
        let mut best = None;
        for (class, _, ch, won) in solver.defender_moves(&st, s, c) {
            let legal = match class {
                Some(k) => hand & s.classes[k].ranks != 0,
                None => hand == 0,
            };
            if !legal {
                continue;
            }
            let won = self.won + u8::from(won);
            let cs = solver.structure(ch.live());
            let layout: usize = cs
                .classes
                .iter()
                .map(|c| c.stride * (self.west & c.ranks).count_ones() as usize)
                .sum();
            let vs = match self.goal {
                Goal::Tricks { target } => solver.feasible_t(ch, target.saturating_sub(won)),
                Goal::Expected => solver.feasible(ch),
            };
            let outcome = vs.iter().map(|v| v.get(layout)).max().unwrap_or(0);
            let key = (outcome, solver.value(ch, self.goal, won).0);
            if best.is_none_or(|(k, _, _, _)| key < k) {
                best = Some((key, class, ch, won));
            }
        }
        let (_, class, ch, won) = best.expect("a defender can always follow or show out");
        let card = class.map(|k| lowest(hand & s.classes[k].ranks));
        if let Some(r) = card {
            self.hands[d] &= !(1 << r);
        }
        self.st = ch;
        self.won = won;
        Play {
            seat: seat(at),
            card: card.map(Rank::new),
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

impl Solver {
    /// 1 on the layouts of `c`
    fn ones(c: Set) -> Tricks {
        let mut v = Tricks::ZERO;
        for i in c.iter() {
            v.set(i, 1);
        }
        v
    }

    /// Pareto-maximal 0/1 vectors (over the layouts consistent with `st`):
    /// 1 where some continuation guarantees at least `need` more tricks.
    /// The same search as [`feasible`](Self::feasible) on vectors collapsed
    /// to a threshold: min and union commute with it, so the answer is
    /// exact, and the antichains stay small where the full ones explode.
    fn feasible_t(&mut self, st: State, need: u8) -> Vec<Tricks> {
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        if need == 0 {
            return vec![Self::ones(c)];
        }
        let remaining =
            st.ns[0].count_ones().max(st.ns[1].count_ones()) as u8 + u8::from(st.played > 0);
        if need > remaining {
            return vec![Tricks::ZERO];
        }
        if let Some(v) = self.memo_t.get(&(st, need)) {
            return v.clone();
        }
        let out = if Self::is_declarer_turn(&st) {
            let moves = self.declarer_moves(&st, &s);
            maximal(
                moves
                    .into_iter()
                    .flat_map(|(ch, _, _)| self.feasible_t(ch, need))
                    .collect(),
                s.len,
            )
        } else {
            let mut acc = vec![Self::ones(c)];
            for (_, g, ch, won) in self.defender_moves(&st, &s, c) {
                let sub_need = need - u8::from(won);
                if sub_need == 0 {
                    continue;
                }
                let map = s.map_to(&self.structure(ch.live()));
                let sub = maximal(
                    self.feasible_t(ch, sub_need)
                        .iter()
                        .map(|y| {
                            let mut p = Tricks::ZERO;
                            for i in g.iter() {
                                p.set(i, y.get(map[i]));
                            }
                            p
                        })
                        .collect(),
                    s.len,
                );
                let mut mask = Tricks::ZERO;
                for i in g.iter() {
                    mask.set(i, 0xFF);
                }
                let words = s.len.div_ceil(8);
                let mut next = Vec::new();
                for x in &acc {
                    next.extend(maximal(
                        sub.iter().map(|y| x.min_on(y, &mask, words)).collect(),
                        s.len,
                    ));
                }
                acc = maximal(next, s.len);
            }
            acc
        };
        self.memo_t.insert((st, need), out.clone());
        out
    }
}
