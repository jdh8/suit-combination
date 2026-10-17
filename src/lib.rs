//! Single-dummy suit-combination solver for contract bridge.
//!
//! Declarer (North–South) sees only the cards played; the defenders see
//! everything and, for their actual layout, pick the card that hurts most.
//! [`analyze`] finds the pure declarer strategy that is best for a [`Goal`]:
//! taking at least some number of tricks in the suit, or the most tricks on
//! average.  Layouts are weighted by vacant places: one giving West `w` of
//! the `n` missing cards has weight `C(26 - n, 13 - w)` out of
//! <code>[TOTAL] = C(26, 13)</code>.  [`Constraints`] set other vacant
//! places, cards known to be with East or West, and ranges of each
//! defender's length and high-card points in the suit; scores are then out
//! of [`Analysis::total`].  They also set where the lead starts, how many
//! entries each hand has outside the suit, how many rounds are played, a
//! fixed first card, and declarer's cards discarded elsewhere.
//!
//! The search is exact.  Instead of a greedy per-branch value — which is *not*
//! optimal here, because a falsecarding defender spreads one layout's weight
//! over several branches — each information state carries the Pareto antichain
//! of "tricks per layout" vectors some continuation guarantees, so the
//! recommended line is the true optimum against omniscient defence.
//!
//! That holds for every trick target and for [`Goal::ExactExpected`].  Only
//! the best vector counts in the end, so the search first finds a good line
//! cheaply, then keeps only the vectors that might still beat it: one that
//! already loses more deals than the good line cannot.  [`Goal::Expected`]
//! instead combines the targets' optimal lines, which is fast, and brackets
//! the true optimum between the line found ([`Analysis::score`]) and the sum
//! of the targets' success probabilities ([`Analysis::bound`]).
//!
//! A search can be stopped early by [`Solver::set_interrupt`]:
//! [`Solver::analyze`] then fails with [`Error::Interrupted`], which still
//! carries an upper bound on the optimum (each trick target's score where
//! its search has finished, else what declarer would make seeing all four
//! hands).  The memo keeps only finished work, so asking again goes on from
//! there.
//!
//! Missing cards with no live declarer card between them are interchangeable:
//! declarer never competes one defender's card against the other's, and a
//! strategy that told them apart could be replaced by a symmetric one no
//! worse.  So a layout only records how many of each such class West holds,
//! and classes merge as the declarer cards separating them are played.
//!
//! A card known to be with one defender is a class of its own: declarer can
//! tell it from its neighbours.  So is each missing honour once HCP are
//! constrained, as which honours a defender holds is no longer symmetric.
//!
//! Assumptions (the usual ones, as in Pavlicek's card-combination tables):
//! unlimited entries to both hands unless [`Constraints::entries`] says
//! otherwise, no ruffs, no inference about the enemy hands beyond
//! [`Constraints`], and only tricks won in this suit count.  The defenders
//! never lead the suit: after winning a round they switch, and declarer
//! comes back by an entry.

use contract_bridge::{Holding, Rank, Seat};
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// `C(26, 13)`: the denominator of every probability without [`Constraints`]
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
    /// Target exceeds the longer hand or [`Constraints::rounds`] (or is zero)
    BadTarget,
    /// West was dealt a card North or South holds, or a hand the
    /// [`Constraints`] rule out
    BadWest,
    /// The card is not one declarer may play now
    NotPlayable,
    /// [`Solver::set_interrupt`] stopped the search.  No line is known, but
    /// no line scores more than `bound` (out of `total`, as
    /// [`Analysis::score`])
    Interrupted { bound: u64, total: u64 },
    /// Known cards that are not missing or are with both defenders, more
    /// than 13 vacant places, a first leader not North or South, no hand
    /// able to lead the first round (a void [`Constraints::first`] and no
    /// entry to the other hand, or no round), a discard not declarer's, or
    /// a fixed lead that is not a card declarer may lead first
    BadConstraints,
    /// No layout fits the [`Constraints`]
    NoLayout,
    /// The goal does not take [`Constraints`] yet: only trick targets do
    Unsupported,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Overlap => "North and South share a card",
            Self::TooManyMissing => "more than 8 cards missing",
            Self::BadTarget => "target must be 1..=longer hand and at most the rounds",
            Self::BadWest => "West holds a card of North or South",
            Self::NotPlayable => "that card cannot be played now",
            Self::BadConstraints => {
                "constraints do not fit the cards: known cards, vacant places, discards or \
                 the first lead"
            }
            Self::NoLayout => "no layout fits the constraints",
            Self::Unsupported => "only trick targets take constraints",
            Self::Interrupted { bound, total } => {
                let bound = *bound as f64 / *total as f64;
                return write!(f, "search interrupted; the best is at most {bound:.4}");
            }
        })
    }
}

impl std::error::Error for Error {}

/// What declarer is playing for
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", rename_all = "snake_case"))]
pub enum Goal {
    /// At least this many tricks in the suit
    Tricks { target: u8 },
    /// The most tricks on average, as well as the trick targets' optimal
    /// lines combine: at every turn declarer may follow one of them or
    /// switch to the best from there.  Usually the optimum, and
    /// [`Analysis::bound`] says when it surely is.
    Expected,
    /// The most tricks on average, by the exact search over full "tricks
    /// per layout" vectors, which can take seconds
    ExactExpected,
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
    /// At least the best score of any line starting with this lead: equal
    /// to `score` except for [`Goal::Expected`], where it is the sum of every
    /// target's score after the lead.  [`Analysis::bound`] is one bound for
    /// every lead; `score == bound` here proves this lead's line the best
    /// after it.
    pub bound: u64,
}

/// Result of [`analyze`]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Analysis {
    pub north: Holding,
    pub south: Holding,
    pub goal: Goal,
    /// For [`Goal::Tricks`], the weight (out of `total`) of the layouts the
    /// optimal line beats; otherwise the line's expected tricks times
    /// `total`
    pub score: u64,
    /// At least the best `score` of any line: equal to it except for
    /// [`Goal::Expected`], where it is the sum of every target's score
    pub bound: u64,
    /// `score == bound`: the line is the best there is.  Otherwise the best
    /// lies in `score..=bound`.
    pub proven: bool,
    pub leads: Vec<Lead>,
    pub plan: Plan,
    /// Weight of every layout: [`TOTAL`] without [`Constraints`]
    pub total: u64,
}

impl Analysis {
    /// Success probability ([`Goal::Tricks`]) or expected tricks
    #[must_use]
    pub fn value(&self) -> f64 {
        self.score as f64 / self.total as f64
    }
}

/// What declarer knows of the defenders' hands besides the cards they play
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Constraints {
    /// Vacant places of East, West: 13 less the cards known in their other
    /// suits.  The default 13/13 means nothing is known.
    pub vacant: [u8; 2],
    /// Cards of the suit known to be with East, West
    pub known: [Holding; 2],
    /// Fewest and most cards of the suit East, West hold
    pub length: [[u8; 2]; 2],
    /// Fewest and most high-card points (A 4, K 3, Q 2, J 1) East, West hold
    /// in this suit; the other suits are not modelled
    pub hcp: [[u8; 2]; 2],
    /// Hand the lead starts in, North or South; `None`: either.  Only
    /// limited [`entries`](Self::entries) make it matter.
    pub first: Option<Seat>,
    /// Entries to North, South in other suits; `None`: unlimited.  A hand
    /// leads free if it won the last round of the suit, or leads the first
    /// round from [`first`](Self::first); otherwise leading from it takes an
    /// entry.  After a defender wins a round, either hand needs one.
    pub entries: [Option<u8>; 2],
    /// Rounds of the suit played before the analysis stops; tricks count
    /// only in these.  13 plays the suit out.
    pub rounds: u8,
    /// Card North or South must lead to the first round
    pub lead: Option<Rank>,
    /// Cards of North, South never played in the suit (SuitPlay's
    /// "discards or ruffs": spent in other suits).  They are not the
    /// defenders' either.
    pub discards: Holding,
}

impl Default for Constraints {
    fn default() -> Self {
        Self {
            vacant: [13; 2],
            known: [Holding::EMPTY; 2],
            length: [[0, 13]; 2],
            hcp: [[0, 10]; 2],
            first: None,
            entries: [None; 2],
            rounds: 13,
            lead: None,
            discards: Holding::EMPTY,
        }
    }
}

/// High-card points of the cards in `mask`
fn points(mask: u16) -> u8 {
    (11..15).map(|r| (mask >> r & 1) as u8 * (r - 10)).sum()
}

impl Constraints {
    /// Honours count apart, so each is a class of its own
    fn splits_honours(&self) -> bool {
        self.hcp != Self::default().hcp
    }

    /// Deals of layout `i` of `classes`, the `missing` cards, per way to
    /// hold its classes' cards: 0 if a length or HCP is out of range, else
    /// the [`vacancy`]
    fn prior(&self, missing: u16, classes: &[Class], i: usize) -> u64 {
        let count = |c: &Class| (i / c.stride % (usize::from(c.size) + 1)) as u8;
        let n = missing.count_ones() as u8;
        let w: u8 = classes.iter().map(count).sum();
        let west: u8 = classes
            .iter()
            .filter(|&c| count(c) > 0)
            .map(|c| points(c.ranks))
            .sum();
        let within = |[lo, hi]: [u8; 2], x| (lo..=hi).contains(&x);
        let fits = within(self.length[0], n - w)
            && within(self.length[1], w)
            && (!self.splits_honours()
                || within(self.hcp[0], points(missing) - west) && within(self.hcp[1], west));
        if fits {
            vacancy(self.vacant, n.into(), w.into())
        } else {
            0
        }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
struct Set([u64; 4]);

impl Set {
    const EMPTY: Self = Self([0; 4]);

    fn insert(&mut self, i: usize) {
        self.0[i >> 6] |= 1 << (i & 63);
    }

    fn is_empty(self) -> bool {
        self.0 == [0; 4]
    }

    fn contains(self, i: usize) -> bool {
        self.0[i >> 6] >> (i & 63) & 1 != 0
    }

    /// Sort key of [`maximal_sets`]: the order [`maximal`] gives the 0/1
    /// [`Tricks`] with the same lanes, which decides ties between lines.
    /// That is more lanes first, then lexicographic by `Tricks` word (eight
    /// lanes, the highest most significant).  Byte `b` of `Set` word `k`
    /// holds the lanes of `Tricks` word `8k + b` in the same significance,
    /// so swapping bytes makes the first of them the most significant.
    fn order(self) -> (std::cmp::Reverse<u32>, [u64; 4]) {
        let count = self.0.iter().map(|w| w.count_ones()).sum();
        (std::cmp::Reverse(count), self.0.map(u64::swap_bytes))
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

impl std::ops::BitOr for Set {
    type Output = Self;
    fn bitor(self, o: Self) -> Self {
        Self(std::array::from_fn(|k| self.0[k] | o.0[k]))
    }
}

impl std::ops::Sub for Set {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self(std::array::from_fn(|k| self.0[k] & !o.0[k]))
    }
}

impl FromIterator<usize> for Set {
    fn from_iter<I: IntoIterator<Item = usize>>(iter: I) -> Self {
        let mut s = Self::EMPTY;
        for i in iter {
            s.insert(i);
        }
        s
    }
}

/// Tricks declarer is guaranteed from here, per layout (0 outside the
/// layouts consistent with the state); eight layouts per word
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// A vector of tricks per layout, as [`maximal`] compares them
trait Lanes: Copy + Eq {
    /// Every lane of `self` is at least the lane of `o` (`len` lanes are in
    /// use, the rest are zero)
    fn covers(&self, o: &Self, len: usize) -> bool;
    /// `self` with the lanes of `branch` (all ones there) zeroed
    fn outside(&self, branch: &Self) -> Self;
}

impl Lanes for Tricks {
    // Lanes stay below 128.
    fn covers(&self, o: &Self, len: usize) -> bool {
        let words = len.div_ceil(8);
        self.0[..words]
            .iter()
            .zip(&o.0[..words])
            .all(|(&v, &r)| ((v | Self::H) - r) & Self::H == Self::H)
    }

    fn outside(&self, branch: &Self) -> Self {
        Self(std::array::from_fn(|k| self.0[k] & !branch.0[k]))
    }
}

/// A 0/1 vector: 1 on the layouts in the set
impl Lanes for Set {
    fn covers(&self, o: &Self, _: usize) -> bool {
        (*o - *self).is_empty()
    }

    fn outside(&self, branch: &Self) -> Self {
        *self - *branch
    }
}

/// Smallest merge worth filing in [`undominated`]: filing every merge made
/// small cases 5% slower on average, some 20%
const GROUP_MIN: usize = 64;

/// Keep, in order, each of `sorted` that no vector kept before it covers.
/// Sorted by decreasing sum, a vector can only be dominated by one before it.
///
/// `branch` marks, at a defender node's merge, the layouts of the branch
/// just merged.  A vector covers another only if its lanes outside the
/// branch cover the other's, so a large merge files the kept vectors by
/// their lanes outside and tests a candidate only against the files whose
/// outside covers its own: the same verdicts from far fewer tests.  Each
/// candidate there is a row `x ∧ y` with `x`'s lanes outside the branch,
/// so files are usually few.
fn undominated<'a, V: Lanes + 'a>(
    sorted: impl ExactSizeIterator<Item = &'a V>,
    len: usize,
    branch: Option<&V>,
) -> Vec<V> {
    let mut out: Vec<V> = Vec::new();
    if let Some(branch) = branch.filter(|_| sorted.len() >= GROUP_MIN) {
        // Lanes outside, and the kept vectors (indices into `out`) with them
        let mut files: Vec<(V, Vec<u32>)> = Vec::new();
        for v in sorted {
            let a = v.outside(branch);
            // A file with `a` itself covers `a`, so the scan meets it.
            let mut same = None;
            let covered = files.iter().enumerate().any(|(k, (b, kept))| {
                if !b.covers(&a, len) {
                    return false;
                }
                if *b == a {
                    same = Some(k);
                }
                kept.iter().any(|&j| out[j as usize].covers(v, len))
            });
            if !covered {
                let j = u32::try_from(out.len()).expect("fewer than 2^32 kept vectors");
                match same {
                    Some(k) => files[k].1.push(j),
                    None => files.push((a, vec![j])),
                }
                out.push(*v);
            }
        }
        return out;
    }
    // The vector that dominated the last candidate usually dominates the next.
    let mut last = 0;
    for v in sorted {
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

/// Drop duplicates and every vector dominated by another; what remains is a
/// Pareto antichain.  `branch`: see [`undominated`].
fn maximal(vs: Vec<Tricks>, len: usize, branch: Option<&Tricks>) -> Vec<Tricks> {
    let words = len.div_ceil(8);
    // Sort an index by sum (descending, then lexicographic for a stable
    // choice of line), not the vectors themselves.
    let mut order: Vec<(u32, u32)> = vs
        .iter()
        .enumerate()
        .map(|(i, v)| (v.sum(len), i as u32))
        .collect();
    order.sort_unstable_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| vs[a.1 as usize].0[..words].cmp(&vs[b.1 as usize].0[..words]))
    });
    // Equal vectors sort next to each other.
    order.dedup_by(|a, b| vs[a.1 as usize].0[..words] == vs[b.1 as usize].0[..words]);
    undominated(order.iter().map(|&(_, i)| &vs[i as usize]), len, branch)
}

/// [`maximal`] on 0/1 vectors as sets: the same antichain in the same order
fn maximal_sets(mut vs: Vec<Set>, len: usize, branch: Option<&Set>) -> Vec<Set> {
    vs.sort_unstable_by_key(|v| v.order());
    vs.dedup();
    undominated(vs.iter(), len, branch)
}

const NORTH: u8 = 0;
const SOUTH: u8 = 2;

/// Vectors kept per merge by [`Cut::Beam`]: 16 to 256 time within 10% of
/// each other on the hard cases, 1024 up to 4x slower
const BEAM: usize = 64;

/// What a search may leave out of an antichain
#[derive(Debug, Clone, Copy)]
enum Cut {
    /// Every vector that loses at most this many deals (see
    /// [`Solver::post`]), and maybe others; `u64::MAX` keeps all
    Budget(u64),
    /// The vectors losing least, this many per merge: some lines' vectors
    /// or less, so the best is a lower bound
    Beam(usize),
}

impl Cut {
    const ALL: Self = Self::Budget(u64::MAX);
}

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
    /// Between tricks, the hands that may lead without an entry (bit 0
    /// North, bit 1 South); 0 in a trick
    lead: u8,
    /// Entries left to North, South; [`UNLIMITED`] never runs out
    entries: [u8; 2],
    /// Rounds left, the current one included; [`UNLIMITED`] when the
    /// limit cannot bind
    rounds: u8,
    /// Card declarer must lead to the first round; 0 for none or later
    fixed: u8,
}

/// [`State::entries`] of a hand with unlimited entries
const UNLIMITED: u8 = u8::MAX;

impl State {
    /// Between tricks, hand `h` (0 North, 1 South) has a card and may lead
    /// it: free, or by an entry
    fn may_lead(&self, h: usize) -> bool {
        self.ns[h] != 0 && (self.lead >> h & 1 != 0 || self.entries[h] > 0)
    }

    /// Between tricks and no round left or no hand may lead: the suit is
    /// over for declarer
    fn is_over(&self) -> bool {
        self.played == 0 && (self.rounds == 0 || !self.may_lead(0) && !self.may_lead(1))
    }

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
    if k > n {
        return 0;
    }
    (0..k).fold(1, |acc, i| acc * (n - i) / (i + 1))
}

/// Ways to fill the `vacant` places (East, West) left by the `n` missing
/// cards, West holding `w` of them, with the defenders' unknown cards of
/// other suits: `C(26 - n, 13 - w)` at 13/13
fn vacancy(vacant: [u8; 2], n: u64, w: u64) -> u64 {
    let [east, west] = vacant.map(u64::from);
    match (west.checked_sub(w), east.checked_sub(n - w)) {
        (Some(rest), Some(other)) => binom(rest + other, rest),
        _ => 0,
    }
}

impl Structure {
    fn new(missing: u16, live: u16, c: &Constraints) -> Self {
        let [east, west] = c.known.map(Holding::to_bits);
        let honours = if c.splits_honours() {
            missing & 0xF << 11
        } else {
            0
        };
        let mut classes: Vec<Class> = Vec::new();
        let mut len = 1;
        let mut push = |ranks: u16| {
            let size = ranks.count_ones() as u8;
            classes.push(Class {
                ranks,
                size,
                stride: len,
            });
            len *= usize::from(size) + 1;
        };
        let mut cur = 0u16;
        for r in 2..16u8 {
            let bit = 1 << r;
            if r < 15 && (east | west | honours) & bit != 0 {
                if cur != 0 {
                    push(cur);
                    cur = 0;
                }
                push(bit);
            } else if r < 15 && missing & bit != 0 {
                cur |= bit;
            } else if cur != 0 && (r == 15 || live & bit != 0) {
                push(cur);
                cur = 0;
            }
        }
        let count = |i: usize, c: &Class| i / c.stride % (usize::from(c.size) + 1);
        // A known card's class holds it on its side: West count 1 or 0
        let fits = |i: usize, c: &Class| {
            (west & c.ranks == 0 || count(i, c) == 1) && (east & c.ranks == 0 || count(i, c) == 0)
        };
        let weight: Vec<u64> = (0..len)
            .map(|i| {
                if !classes.iter().all(|c| fits(i, c)) {
                    return 0;
                }
                classes
                    .iter()
                    .map(|c| binom(c.size.into(), count(i, c) as u64))
                    .product::<u64>()
                    * c.prior(missing, &classes, i)
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
        for i in (0..len).filter(|&i| weight[i] > 0) {
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

/// Tricks declarer takes from here, between tricks, with every card in
/// view: `hands` are the cards of North, East, South, West, and `memo` maps
/// the cards still out (which, in one deal, fix every hand) to the answer
fn dd_tricks(hands: [u16; 4], memo: &mut FxMap<u16, u8>) -> u8 {
    if hands[0] | hands[2] == 0 {
        return 0;
    }
    let out = hands.iter().fold(0, |acc, h| acc | h);
    if let Some(&t) = memo.get(&out) {
        return t;
    }
    let t = [0, 2]
        .into_iter()
        .filter(|&at| hands[at] != 0)
        .map(|at| dd_trick(hands, at, [0; 4], 0, memo))
        .max()
        .unwrap_or(0);
    memo.insert(out, t);
    t
}

/// [`dd_tricks`] within the trick `leader` led, with `trick[..played]` on
/// the table.  A hand plays only the lowest of cards with nothing out or on
/// the table between them; a void hand discards (rank 0).
fn dd_trick(
    hands: [u16; 4],
    leader: usize,
    trick: [u8; 4],
    played: usize,
    memo: &mut FxMap<u16, u8>,
) -> u8 {
    if played == 4 {
        let pos = (0..4).max_by_key(|&p| trick[p]).unwrap();
        return u8::from((leader + pos).is_multiple_of(2)) + dd_tricks(hands, memo);
    }
    let at = (leader + played) % 4;
    let hand = hands[at];
    let deck = trick
        .iter()
        .fold(hands.iter().fold(0, |a, h| a | h), |a, &r| a | 1 << r)
        & !1;
    let options = if hand == 0 {
        1
    } else {
        ranks(hand)
            .filter(|&r| {
                let below = deck & ((1 << r) - 1);
                below == 0 || hand >> (15 - below.leading_zeros()) & 1 == 0
            })
            .fold(0, |a, r| a | 1 << r)
    };
    let outcomes = (0..15).filter(|r| options >> r & 1 != 0).map(|r| {
        let mut h = hands;
        h[at] &= !(1 << r);
        let mut t = trick;
        t[played] = r;
        dd_trick(h, leader, t, played + 1, memo)
    });
    if at.is_multiple_of(2) {
        outcomes.max()
    } else {
        outcomes.min()
    }
    .unwrap()
}

/// rustc's FxHasher: one multiply per word, for the memo tables' small,
/// trusted keys.  The final rotation (as in rustc-hash 2) brings the
/// well-mixed high bits down to the bucket index.
#[derive(Default)]
struct Fx(u64);

impl Fx {
    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl std::hash::Hasher for Fx {
    #[inline]
    fn finish(&self) -> u64 {
        self.0.rotate_left(26)
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks();
        for &w in words {
            self.add(u64::from_le_bytes(w));
        }
        if !rest.is_empty() {
            let mut w = [0; 8];
            w[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(w));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(i.into());
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add(i.into());
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
}

type FxMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<Fx>>;

/// The search for one combination, memoized across goals and [`Game`]s
pub struct Solver {
    north: Holding,
    south: Holding,
    missing: u16,
    constraints: Constraints,
    /// Layout spaces by live declarer cards
    structures: FxMap<u16, Rc<Structure>>,
    /// Antichains of tricks per layout, for [`Goal::ExactExpected`], with
    /// the [`Cut::Budget`] they hold
    memo: FxMap<State, (u64, Vec<Tricks>)>,
    /// Antichains of 0/1 per layout by tricks still needed, for trick
    /// targets, with the [`Cut::Budget`] they hold
    memo_t: FxMap<(State, u8), (u64, Vec<Set>)>,
    /// [`Cut::Beam`] answers that left something out
    beam: FxMap<State, Vec<Tricks>>,
    beam_t: FxMap<(State, u8), Vec<Set>>,
    /// [`Structure::map_to`] by the live cards of parent and child
    maps: FxMap<(u16, u16), Rc<[usize]>>,
    /// [`Goal::Expected`] lines by state and requirement followed
    memo_line: FxMap<(State, Tricks), (Tricks, Step)>,
    /// See [`set_interrupt`](Self::set_interrupt)
    interrupt: Option<Box<dyn FnMut(usize) -> bool>>,
    /// Memo misses between polls of `interrupt`, and since the last poll
    poll_every: u32,
    misses: u32,
    /// `analyze` is running with an interrupt: memo misses poll it
    armed: bool,
    /// The interrupt fired: every search returns at once and memoizes nothing
    interrupted: bool,
    /// Double-dummy tricks per root layout, once an interrupt needs them
    dd: Option<Rc<[u8]>>,
}

/// Memo misses between polls of [`Solver::set_interrupt`]'s callback
const POLL_EVERY: u32 = 1024;

/// How a [`Goal::Expected`] line goes on from a state
#[derive(Debug, Clone, Copy)]
enum Step {
    /// No more tricks to be had
    Done,
    /// Adopt this target's vector of [`Solver::adopt`]
    Adopt(usize),
    /// Play this move of [`Solver::declarer_moves`]
    Move(usize),
    /// A defender plays; the requirement passes to each branch
    Branch,
}

impl Solver {
    pub fn new(north: Holding, south: Holding) -> Result<Self, Error> {
        Self::with_constraints(north, south, Constraints::default())
    }

    /// A solver for `north` opposite `south` under `constraints`, which only
    /// trick targets take ([`Error::Unsupported`] otherwise)
    pub fn with_constraints(
        north: Holding,
        south: Holding,
        constraints: Constraints,
    ) -> Result<Self, Error> {
        if !(north & south).is_empty() {
            return Err(Error::Overlap);
        }
        let missing = (Holding::ALL - north - south).to_bits();
        if missing.count_ones() as usize > MAX_MISSING {
            return Err(Error::TooManyMissing);
        }
        let [east, west] = constraints.known.map(Holding::to_bits);
        if (east | west) & !missing != 0
            || east & west != 0
            || constraints.vacant.iter().any(|&v| v > 13)
            || !matches!(constraints.first, None | Some(Seat::North | Seat::South))
            || !(constraints.discards - north - south).is_empty()
        {
            return Err(Error::BadConstraints);
        }
        let mut solver = Self {
            north,
            south,
            missing,
            constraints,
            structures: FxMap::default(),
            memo: FxMap::default(),
            memo_t: FxMap::default(),
            beam: FxMap::default(),
            beam_t: FxMap::default(),
            maps: FxMap::default(),
            memo_line: FxMap::default(),
            interrupt: None,
            poll_every: POLL_EVERY,
            misses: 0,
            armed: false,
            interrupted: false,
            dd: None,
        };
        let root = solver.root();
        let leadable = |r: Rank| {
            let r = r.get();
            (0..2).any(|h| root.ns[h] >> r & 1 != 0 && root.may_lead(h))
        };
        if root.is_over() || constraints.lead.is_some_and(|r| !leadable(r)) {
            return Err(Error::BadConstraints);
        }
        if solver.total() == 0 {
            return Err(Error::NoLayout);
        }
        Ok(solver)
    }

    /// Weight of every layout, the denominator of every score:
    /// [`TOTAL`] without [`Constraints`]
    pub fn total(&mut self) -> u64 {
        let s = self.structure(self.root().live());
        s.weight.iter().sum()
    }

    /// Let `f` stop [`analyze`](Self::analyze) early.  It is called every
    /// 1024 or so states searched, with the number of states memoized (a
    /// proxy for memory), and returning `true` makes `analyze` fail with
    /// [`Error::Interrupted`].  Nothing half-searched is memoized, so the
    /// next call goes on from where this one stopped.  [`Game`] never
    /// polls it.
    // ponytail: polled between states, so one state whose antichains take
    // seconds to combine (KJ9753 opposite a void) overruns by that much;
    // killing the thread or worker is the hard stop.
    pub fn set_interrupt(&mut self, f: impl FnMut(usize) -> bool + 'static) {
        self.interrupt = Some(Box::new(f));
    }

    /// Search to the end again
    pub fn clear_interrupt(&mut self) {
        self.interrupt = None;
    }

    /// Poll the interrupt every `n` memo misses instead of 1024, so tests
    /// can stop small searches anywhere
    #[doc(hidden)]
    pub fn set_poll_interval(&mut self, n: u32) {
        self.poll_every = n.max(1);
    }

    /// Count a memo miss, polling the interrupt when due.  True once it has
    /// fired: the caller returns at once.
    fn tick(&mut self) -> bool {
        if self.armed && !self.interrupted {
            self.misses += 1;
            if self.misses >= self.poll_every {
                self.misses = 0;
                let states = self.memo.len() + self.memo_t.len() + self.memo_line.len();
                self.interrupted = self.interrupt.as_mut().is_some_and(|f| f(states));
            }
        }
        self.interrupted
    }

    /// Diagnostics for benchmarks: memoized states and the largest antichain
    #[doc(hidden)]
    pub fn stats(&self) -> (usize, usize) {
        let states = self.memo.len() + self.memo_t.len() + self.memo_line.len();
        let widest = self
            .memo
            .values()
            .map(|(_, v)| v.len())
            .chain(self.memo_t.values().map(|(_, v)| v.len()))
            .max()
            .unwrap_or(0);
        (states, widest)
    }

    fn structure(&mut self, live: u16) -> Rc<Structure> {
        Rc::clone(
            self.structures
                .entry(live)
                .or_insert_with(|| Rc::new(Structure::new(self.missing, live, &self.constraints))),
        )
    }

    /// For each layout of `st`, the layout of `ch` it becomes: a cached
    /// [`Structure::map_to`], as a structure depends only on the live cards
    fn map(&mut self, st: &State, ch: &State) -> Rc<[usize]> {
        let key = (st.live(), ch.live());
        if let Some(map) = self.maps.get(&key) {
            return Rc::clone(map);
        }
        let map: Rc<[usize]> = self.structure(key.0).map_to(&self.structure(key.1)).into();
        self.maps.insert(key, Rc::clone(&map));
        map
    }

    /// Most tricks declarer can take: the longer hand's cards in play, or
    /// fewer rounds
    fn max_tricks(&self) -> u8 {
        let st = self.root();
        let longer = st.ns[0].count_ones().max(st.ns[1].count_ones()) as u8;
        longer.min(self.constraints.rounds)
    }

    fn check(&self, goal: Goal) -> Result<(), Error> {
        match goal {
            Goal::Tricks { target } if target == 0 || target > self.max_tricks() => {
                Err(Error::BadTarget)
            }
            Goal::Tricks { .. } => Ok(()),
            _ if self.constraints != Constraints::default() => Err(Error::Unsupported),
            _ => Ok(()),
        }
    }

    fn root(&self) -> State {
        let c = &self.constraints;
        let ns = [self.north, self.south].map(|h| (h - c.discards).to_bits());
        let longer = ns[0].count_ones().max(ns[1].count_ones()) as u8;
        let entries = c.entries.map(|e| e.unwrap_or(UNLIMITED));
        let unlimited = u8::from(entries[0] == UNLIMITED) | u8::from(entries[1] == UNLIMITED) << 1;
        State {
            ns,
            shown: [[0; MAX_MISSING]; 2],
            void: [false; 2],
            leader: NORTH,
            trick: [0; 4],
            played: 0,
            lead: match c.first {
                Some(Seat::North) => 1 | unlimited,
                Some(Seat::South) => 2 | unlimited,
                _ => 3,
            },
            entries,
            // Each round takes a card of the longer hand
            rounds: if c.rounds >= longer {
                UNLIMITED
            } else {
                c.rounds
            },
            fixed: c.lead.map_or(0, Rank::get),
        }
    }

    /// Goal score of guaranteeing `v` more tricks after `won`, over the
    /// layouts `c`
    fn score(s: &Structure, c: Set, goal: Goal, won: u8, v: &Tricks) -> u64 {
        c.iter()
            .map(|i| match goal {
                Goal::Tricks { target } => u64::from(v.get(i) + won >= target) * s.weight[i],
                Goal::Expected | Goal::ExactExpected => u64::from(v.get(i) + won) * s.weight[i],
            })
            .sum()
    }

    /// Some continuation from `st` wins at least `req` tricks per layout
    fn guarantees(&mut self, st: State, req: &Tricks, len: usize, goal: Goal) -> bool {
        // A vector covering `req` loses no more than `req` does.
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        match goal {
            Goal::ExactExpected => {
                let budget = match self.memo.get(&st) {
                    Some(&(u64::MAX, _)) => u64::MAX,
                    _ => Self::lost(&self.post(&st, &s), c, Self::cap(&st), req),
                };
                let (vs, _) = self.feasible(st, Cut::Budget(budget));
                vs.iter().any(|v| v.covers(req, len))
            }
            // Every nonzero lane of `req` holds the same count on this path.
            // A Goal::Expected line follows trick targets' vectors.
            Goal::Tricks { .. } | Goal::Expected => {
                let need = (0..len).map(|i| req.get(i)).max().unwrap_or(0);
                let req: Set = (0..len).filter(|&i| req.get(i) > 0).collect();
                let budget = match self.memo_t.get(&(st, need)) {
                    Some(&(u64::MAX, _)) => u64::MAX,
                    _ => Self::deals(&self.post(&st, &s), c - req),
                };
                let (vs, _) = self.feasible_t(st, need, Cut::Budget(budget));
                vs.iter().any(|v| v.covers(&req, len))
            }
        }
    }

    /// Best [`score`](Self::score) some continuation from `st` reaches,
    /// with the vector that reaches it: tricks per layout for expected
    /// tricks, 0/1 per layout for a trick target
    fn value(&mut self, st: State, goal: Goal, won: u8) -> (u64, Tricks) {
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        // A beam finds some line's score `x` (or the whole antichain).  The
        // best line scores no less, so it falls short of `all` by at most
        // `all - x`, which bounds the deals it loses (see `budget`).  The
        // search keeping every vector within that loss finds it, and what
        // else it returns scores below `x`.
        match goal {
            Goal::Tricks { target } => {
                let need = target.saturating_sub(won);
                let all: u64 = c.iter().map(|i| s.weight[i]).sum();
                let top = |vs: Vec<Set>| {
                    vs.into_iter()
                        .map(|v| ((c & v).iter().map(|i| s.weight[i]).sum(), v))
                        .max_by_key(|&(x, _)| x)
                };
                let (vs, done) = self.feasible_t(st, need, Cut::Beam(BEAM));
                let mut best = top(vs);
                if !done {
                    let budget = self.budget(&st, &s, all - best.map_or(0, |(x, _)| x));
                    best = top(self.feasible_t(st, need, Cut::Budget(budget)).0);
                }
                best.map_or((0, Tricks::ZERO), |(x, v)| (x, Self::ones(v)))
            }
            Goal::ExactExpected => {
                let all: u64 = c
                    .iter()
                    .map(|i| s.weight[i] * u64::from(Self::cap(&st) + won))
                    .sum();
                let top = |vs: Vec<Tricks>| {
                    vs.into_iter()
                        .map(|v| (Self::score(&s, c, goal, won, &v), v))
                        .max_by_key(|&(x, _)| x)
                };
                let (vs, done) = self.feasible(st, Cut::Beam(BEAM));
                let mut best = top(vs);
                if !done {
                    let budget = self.budget(&st, &s, all - best.map_or(0, |(x, _)| x));
                    best = top(self.feasible(st, Cut::Budget(budget)).0);
                }
                best.unwrap_or((0, Tricks::ZERO))
            }
            Goal::Expected => {
                let t = self.line(st, Tricks::ZERO);
                (Self::score(&s, c, goal, won, &t), t)
            }
        }
    }

    /// [`Analysis`] for every target from one trick up to the longer hand's
    /// length, then for [`Goal::Expected`] unless there are [`Constraints`];
    /// the first goal interrupted ends it
    pub fn analyze_all(&mut self) -> Result<Vec<Analysis>, Error> {
        let ev = self.constraints == Constraints::default();
        (1..=self.max_tricks())
            .map(|target| Goal::Tricks { target })
            .chain(ev.then_some(Goal::Expected))
            .map(|goal| self.analyze(goal))
            .collect()
    }

    /// Best line for `goal`, or [`Error::Interrupted`] if the interrupt
    /// (see [`set_interrupt`](Self::set_interrupt)) fires first
    pub fn analyze(&mut self, goal: Goal) -> Result<Analysis, Error> {
        self.check(goal)?;
        self.armed = self.interrupt.is_some();
        self.misses = 0;
        let a = self.search(goal);
        self.armed = false;
        if std::mem::take(&mut self.interrupted) {
            return Err(Error::Interrupted {
                bound: self.ceiling(goal),
                total: self.total(),
            });
        }
        a
    }

    /// [`analyze`](Self::analyze) without the bookkeeping of interrupts
    fn search(&mut self, goal: Goal) -> Result<Analysis, Error> {
        let root = self.root();
        let structure = self.structure(root.live());
        let c = Self::consistent(&root, &structure);
        let moves = self.declarer_moves(&root, &structure);
        let reqs = match goal {
            Goal::Expected => self.adopt(root, c),
            _ => Vec::new(),
        };
        let mut leads = Vec::new();
        let mut best: Option<(u64, Tricks, usize)> = None;
        for (i, &(st, at, cards)) in moves.iter().enumerate() {
            let (score, v) = match goal {
                Goal::Expected => {
                    let (t, r) = self.best_line(st, &reqs);
                    (Self::score(&structure, c, goal, 0, &t), r)
                }
                _ => self.value(st, goal, 0),
            };
            let bound = match goal {
                Goal::Expected => self.target_sum(st, 0),
                _ => score,
            };
            leads.push(Lead {
                seat: seat(at),
                cards: Holding::from_bits_truncate(cards),
                score,
                bound,
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
        let then = match goal {
            Goal::Expected => self.plan_line(moves[i].0, req),
            _ => self.plan(moves[i].0, req, goal),
        };
        let plan = Plan::Declarer {
            seat: leads[i].seat,
            cards: leads[i].cards,
            then: Box::new(then),
        };
        let bound = match goal {
            Goal::Expected => self.target_sum(root, 0),
            _ => score,
        };
        Ok(Analysis {
            north: self.north,
            south: self.south,
            goal,
            score,
            bound,
            proven: score == bound,
            leads,
            plan,
            total: structure.weight.iter().sum(),
        })
    }

    /// Sum over the targets of their best score from `st` after `won`
    /// tricks: no line from there makes more tricks on average
    fn target_sum(&mut self, st: State, won: u8) -> u64 {
        (1..=self.max_tricks())
            .map(|target| self.value(st, Goal::Tricks { target }, won).0)
            .sum()
    }

    /// Upper bound on the best score for `goal` from what an interrupted
    /// search has finished: a target's score if its search is done, else
    /// its double-dummy score; for expected tricks, the sum over targets
    fn ceiling(&mut self, goal: Goal) -> u64 {
        let root = self.root();
        let s = self.structure(root.live());
        let dd = self.double_dummy();
        let max = self.max_tricks();
        let mut target = |target: u8| {
            if self.memo_t.contains_key(&(root, target)) {
                self.value(root, Goal::Tricks { target }, 0).0
            } else {
                (0..s.len)
                    .filter(|&i| dd[i] >= target)
                    .map(|i| s.weight[i])
                    .sum()
            }
        };
        match goal {
            Goal::Tricks { target: t } => target(t),
            _ => (1..=max).map(target).sum(),
        }
    }

    /// Tricks declarer takes on each root layout seeing all four hands,
    /// which no line beats.  One representative deal per layout: missing
    /// cards of a class are interchangeable.
    fn double_dummy(&mut self) -> Rc<[u8]> {
        if let Some(dd) = &self.dd {
            return Rc::clone(dd);
        }
        let s = self.structure(self.root().live());
        let mut memo = FxMap::default();
        let dd: Rc<[u8]> = (0..s.len)
            .map(|i| {
                let west = s.classes.iter().fold(0, |acc, c| {
                    let k = i / c.stride % (usize::from(c.size) + 1);
                    acc | ranks(c.ranks).take(k).fold(0, |a, r| a | 1 << r)
                });
                let [north, south] = self.root().ns;
                let hands = [north, self.missing & !west, south, west];
                memo.clear();
                dd_tricks(hands, &mut memo)
            })
            .collect();
        self.dd = Some(Rc::clone(&dd));
        dd
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
        let winner = (st.leader + pos as u8) % 4;
        let won = winner.is_multiple_of(2);
        // A hand with unlimited entries always leads free
        st.lead = st.entries.iter().enumerate().fold(0, |acc, (h, &e)| {
            acc | u8::from(e == UNLIMITED || won && usize::from(winner / 2) == h) << h
        });
        st.trick = [0; 4];
        st.played = 0;
        st.leader = NORTH;
        if st.rounds != UNLIMITED {
            st.rounds -= 1;
        }
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
            let h = usize::from(at / 2);
            if st.lead >> h & 1 == 0 && st.entries[h] != UNLIMITED {
                ch.entries[h] -= 1;
            }
            ch.leader = at;
            ch.lead = 0;
            ch.fixed = 0;
        }
        ch.ns[at as usize / 2] &= !(1 << rank);
        self.advance(ch, s, rank).0
    }

    /// Declarer's options now: (resulting state, seat, equivalence class
    /// played).  Between tricks that includes the choice of hand, among
    /// those [`State::may_lead`]; at third hand a void hand has the single
    /// option "discard" (class 0).
    fn declarer_moves(&mut self, st: &State, s: &Structure) -> Vec<(State, u8, u16)> {
        let mut out = Vec::new();
        let seats: &[u8] = if st.played == 0 {
            &[NORTH, SOUTH]
        } else {
            &[(st.leader + st.played) % 4]
        };
        for &at in seats {
            if st.played == 0 && !st.may_lead(usize::from(at / 2)) {
                continue;
            }
            let hand = st.ns[at as usize / 2];
            if hand == 0 {
                if st.played != 0 {
                    out.push((self.declarer_play(st, s, at, 0), at, 0));
                }
                continue;
            }
            for class in Self::classes(st, s, hand) {
                if st.fixed != 0 && class >> st.fixed & 1 == 0 {
                    continue;
                }
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
    /// consistent with `st`) that some declarer continuation guarantees, as
    /// far as `cut` keeps them, and whether it kept them all
    fn feasible(&mut self, st: State, cut: Cut) -> (Vec<Tricks>, bool) {
        match (self.memo.get(&st), cut) {
            (Some((b, v)), Cut::Budget(budget)) if *b >= budget => {
                return (v.clone(), *b == u64::MAX);
            }
            // A smaller budget may have kept nothing worth a beam.
            (Some((u64::MAX, v)), Cut::Beam(_)) => return (v.clone(), true),
            _ => {}
        }
        if let Cut::Beam(_) = cut
            && let Some(v) = self.beam.get(&st)
        {
            return (v.clone(), false);
        }
        if self.tick() {
            return (Vec::new(), false);
        }
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        let cap = Self::cap(&st);
        let mut w = match cut {
            Cut::Budget(b) if b != u64::MAX => self.post(&st, &s),
            _ => Vec::new(),
        };
        let budget = match cut {
            Cut::Budget(b) if b != u64::MAX && b < Self::lost(&w, c, cap, &Tricks::ZERO) => b,
            _ => u64::MAX,
        };
        let cut = match cut {
            Cut::Budget(_) => Cut::Budget(budget),
            beam => beam,
        };
        let mut kept = true;
        let out = if st.is_over() {
            vec![Tricks::ZERO]
        } else if Self::is_declarer_turn(&st) {
            let mut all = Vec::new();
            for (ch, _, _) in self.declarer_moves(&st, &s) {
                let (vs, whole) = self.feasible(ch, cut);
                if self.interrupted {
                    return (Vec::new(), false);
                }
                kept &= whole;
                all.extend(vs);
            }
            let mut out = maximal(all, s.len, None);
            out.retain(|v| budget == u64::MAX || Self::lost(&w, c, cap, v) <= budget);
            if matches!(cut, Cut::Beam(k) if out.len() > k) && w.is_empty() {
                w = self.post(&st, &s);
            }
            Self::trim(out, cut, &mut kept, |v| Self::lost(&w, c, cap, v))
        } else {
            // The defender routes each layout to its worst legal branch, so a
            // continuation guarantees per layout the minimum over the
            // branches where that layout may legally play.  A completed trick
            // may coarsen the structure; `map` lifts the child's vectors.
            // Every vector is at most the minimum over branches of the most
            // each branch holds, so the merge starts there.
            let mut start = Tricks::ZERO;
            for i in c.iter() {
                start.set(i, 127);
            }
            let mut subs = Vec::new();
            for (_, g, ch, won) in self.defender_moves(&st, &s, c) {
                let map = self.map(&st, &ch);
                let bonus = u8::from(won);
                let (vs, whole) = self.feasible(ch, cut);
                if self.interrupted {
                    return (Vec::new(), false);
                }
                kept &= whole;
                // Only the branch's legal layouts matter, so project onto
                // them first: that collapses many continuations into one.
                let sub = maximal(
                    vs.iter()
                        .map(|y| {
                            let mut p = Tricks::ZERO;
                            for i in g.iter() {
                                p.set(i, y.get(map[i]) + bonus);
                            }
                            p
                        })
                        .filter(|p| budget == u64::MAX || Self::lost(&w, g, cap, p) <= budget)
                        .collect(),
                    s.len,
                    None,
                );
                for i in g.iter() {
                    let most = sub.iter().map(|p| p.get(i)).max().unwrap_or(0);
                    start.set(i, start.get(i).min(most));
                }
                let most = if budget == u64::MAX {
                    0
                } else {
                    sub.iter().map(|p| Self::held(&w, g, p)).max().unwrap_or(0)
                };
                let mut mask = Tricks::ZERO;
                for i in g.iter() {
                    mask.set(i, 0xFF);
                }
                subs.push((g, mask, sub, most));
            }
            if budget != u64::MAX {
                subs.sort_by_key(|(_, _, sub, _)| sub.len());
            }
            let words = s.len.div_ceil(8);
            let mut acc = vec![start];
            for b in 0..subs.len() {
                let (_, mask, ref sub, _) = subs[b];
                // A vector loses at least what it holds in a later branch
                // beyond the most any vector there holds.
                let keep = |r: &Tricks| {
                    budget == u64::MAX
                        || Self::lost(&w, c, cap, r)
                            + subs[b + 1..]
                                .iter()
                                .map(|&(h, _, _, most)| Self::held(&w, h, r).saturating_sub(most))
                                .max()
                                .unwrap_or(0)
                            <= budget
                };
                let mut next = Vec::new();
                for x in &acc {
                    // ponytail: pre-reduce each row; the final filter sees far fewer
                    next.extend(maximal(
                        sub.iter()
                            .map(|y| x.min_on(y, &mask, words))
                            .filter(keep)
                            .collect(),
                        s.len,
                        None,
                    ));
                }
                acc = maximal(next, s.len, Some(&mask));
                if matches!(cut, Cut::Beam(k) if acc.len() > k) && w.is_empty() {
                    w = self.post(&st, &s);
                }
                acc = Self::trim(acc, cut, &mut kept, |v| Self::lost(&w, c, cap, v));
            }
            acc
        };
        match cut {
            Cut::Beam(_) if !kept => {
                self.beam.insert(st, out.clone());
            }
            _ => {
                self.memo.insert(st, (budget, out.clone()));
            }
        }
        (out, kept && budget == u64::MAX)
    }

    /// `vs` cut to a [`Cut::Beam`]'s width, those losing least first
    fn trim<V>(mut vs: Vec<V>, cut: Cut, kept: &mut bool, lost: impl Fn(&V) -> u64) -> Vec<V> {
        if let Cut::Beam(k) = cut
            && vs.len() > k
        {
            vs.sort_by_cached_key(lost);
            vs.truncate(k);
            *kept = false;
        }
        vs
    }

    /// Deals consistent with the cards seen, per layout of `st` (0 off the
    /// consistent ones).  A class of `m` cards of which West showed `sw` and
    /// East `se` leaves `C(m - sw - se, k - sw)` ways for West to hold `k`:
    /// the deals in which the very cards shown sit where they were shown.
    /// A line failing a layout here fails at least these deals at the root,
    /// however play got here, and a defender's card leaves each layout of
    /// the child at most the deals of the parent's layouts that become it.
    /// So a vector losing more deals here than a known line loses at the
    /// root can be dropped: nothing built on it beats that line.
    fn post(&self, st: &State, s: &Structure) -> Vec<u64> {
        let mut w = vec![0; s.len];
        for i in Self::consistent(st, s).iter() {
            let mut deals = 1;
            for (k, class) in s.classes.iter().enumerate() {
                let held = (i / class.stride % (usize::from(class.size) + 1)) as u64;
                let (sw, se) = (u64::from(st.shown[1][k]), u64::from(st.shown[0][k]));
                deals *= binom(u64::from(class.size) - sw - se, held - sw);
            }
            w[i] = deals * self.constraints.prior(self.missing, &s.classes, i);
        }
        w
    }

    /// The most deals of [`post`](Self::post) a vector from `st` can lose
    /// while losing at most `short` of the score: a layout weighs at least
    /// its deals, by a ratio that varies, so take the largest
    fn budget(&self, st: &State, s: &Structure, short: u64) -> u64 {
        let w = self.post(st, s);
        Self::consistent(st, s)
            .iter()
            .map(|i| {
                let b = (u128::from(short) * u128::from(w[i])).div_ceil(u128::from(s.weight[i]));
                u64::try_from(b).expect("deals are at most the weight")
            })
            .max()
            .unwrap_or(0)
    }

    /// Deals of `w` on the layouts of `c`
    fn deals(w: &[u64], c: Set) -> u64 {
        c.iter().map(|i| w[i]).sum()
    }

    /// Deals of `w` times the tricks `v` falls short of `cap`, over `c`: an
    /// expected-tricks vector's loss
    fn lost(w: &[u64], c: Set, cap: u8, v: &Tricks) -> u64 {
        c.iter()
            .map(|i| w[i] * u64::from(cap.saturating_sub(v.get(i))))
            .sum()
    }

    /// Deals of `w` times the tricks of `v`, over `c`
    fn held(w: &[u64], c: Set, v: &Tricks) -> u64 {
        c.iter().map(|i| w[i] * u64::from(v.get(i))).sum()
    }

    /// Tricks left to play, the current one included: the longer hand plays
    /// to each
    fn cap(st: &State) -> u8 {
        let mut n = st.ns.map(|h| h.count_ones() as u8);
        for p in 0..usize::from(st.played) {
            let at = (st.leader + p as u8) % 4;
            if at.is_multiple_of(2) && st.trick[p] != 0 {
                n[usize::from(at / 2)] += 1;
            }
        }
        n[0].max(n[1])
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
            assert!(self.interrupted, "req is feasible, so some move keeps it");
            return Plan::Hopeless;
        }
        let seat_index = (st.leader + st.played) % 4;
        let mut branches: Vec<(Vec<Option<Rank>>, Plan)> = Vec::new();
        for (class, g, ch, won) in self.defender_moves(&st, &s, c) {
            let map = self.map(&st, &ch);
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
            Self::add_branch(&mut branches, &s, class, sub);
        }
        Plan::Defender {
            seat: seat(seat_index),
            branches,
        }
    }

    /// Add a defender's `class` (`None` = shows out) leading to `sub`,
    /// merged with any branch that goes on the same way
    fn add_branch(
        branches: &mut Vec<(Vec<Option<Rank>>, Plan)>,
        s: &Structure,
        class: Option<usize>,
        sub: Plan,
    ) {
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
    /// For [`Goal::Expected`], the target vectors (in tricks still needed)
    /// that declarer's play so far still follows
    reqs: Vec<Tricks>,
}

impl Game {
    /// Deal `west` the given cards (East gets the rest of the missing ones)
    /// and play for `goal`
    pub fn new(solver: &mut Solver, west: Holding, goal: Goal) -> Result<Self, Error> {
        solver.check(goal)?;
        let west = west.to_bits();
        if west & !solver.missing != 0 {
            return Err(Error::BadWest);
        }
        let st = solver.root();
        let s = solver.structure(st.live());
        let layout: usize = s
            .classes
            .iter()
            .map(|c| c.stride * (west & c.ranks).count_ones() as usize)
            .sum();
        if s.weight[layout] == 0 {
            return Err(Error::BadWest);
        }
        let reqs = match goal {
            Goal::Expected => {
                let s = solver.structure(st.live());
                solver.adopt(st, Solver::consistent(&st, &s))
            }
            _ => Vec::new(),
        };
        Ok(Self {
            goal,
            st,
            west,
            hands: [solver.missing & !west, west],
            won: 0,
            reqs,
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

    /// The suit is played out, or declarer may not lead it again
    pub fn is_over(&self) -> bool {
        self.st.is_over()
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
        let c = Solver::consistent(&self.st, &s);
        solver
            .declarer_moves(&self.st, &s)
            .into_iter()
            .map(|(ch, at, cards)| {
                let (score, bound) = match self.goal {
                    Goal::Expected => {
                        let (t, _) = solver.best_line(ch, &self.reqs);
                        let score = Solver::score(&s, c, self.goal, self.won, &t);
                        (score, solver.target_sum(ch, self.won))
                    }
                    _ => {
                        let score = solver.value(ch, self.goal, self.won).0;
                        (score, score)
                    }
                };
                Lead {
                    seat: seat(at),
                    cards: Holding::from_bits_truncate(cards),
                    score,
                    bound,
                }
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
        let st = self.st;
        self.reqs
            .retain(|r| solver.guarantees(st, r, s.len, Goal::Expected));
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
        let moves = solver.defender_moves(&st, s, c);
        let mut best: Option<((u8, u64), usize)> = None;
        for (m, &(class, _, ch, trick)) in moves.iter().enumerate() {
            let legal = match class {
                Some(k) => hand & s.classes[k].ranks != 0,
                None => hand == 0,
            };
            if !legal {
                continue;
            }
            let won = self.won + u8::from(trick);
            let cs = solver.structure(ch.live());
            let layout: usize = cs
                .classes
                .iter()
                .map(|c| c.stride * (self.west & c.ranks).count_ones() as usize)
                .sum();
            let outcome = match self.goal {
                Goal::Tricks { target } => {
                    let (vs, _) = solver.feasible_t(ch, target.saturating_sub(won), Cut::ALL);
                    vs.iter().any(|v| v.contains(layout)).into()
                }
                Goal::ExactExpected => {
                    let (vs, _) = solver.feasible(ch, Cut::ALL);
                    vs.iter().map(|v| v.get(layout)).max().unwrap_or(0)
                }
                // The most tricks declarer could be sure of on this layout,
                // which the 0/1 searches answer exactly
                Goal::Expected => (1..)
                    .take_while(|&k| {
                        solver
                            .feasible_t(ch, k, Cut::ALL)
                            .0
                            .iter()
                            .any(|v| v.contains(layout))
                    })
                    .count() as u8,
            };
            let key = (outcome, solver.value(ch, self.goal, won).0);
            if best.is_none_or(|(k, _)| key < k) {
                best = Some((key, m));
            }
        }
        let (_, m) = best.expect("a defender can always follow or show out");
        let (class, g, ch, trick) = moves[m];
        let map = solver.map(&st, &ch);
        self.reqs = self
            .reqs
            .iter()
            .map(|r| Solver::pass(r, g, &map, trick))
            .filter(|r| *r != Tricks::ZERO)
            .collect();
        self.reqs.dedup();
        let won = self.won + u8::from(trick);
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

    /// Pareto-maximal 0/1 vectors, as sets of layouts (of those consistent
    /// with `st`) where some continuation guarantees at least `need` more
    /// tricks, as far as `cut` keeps them, and whether it kept them all.
    /// The same search as [`feasible`](Self::feasible) on vectors collapsed
    /// to a threshold: min and union commute with it, so the answer is
    /// exact, and the antichains stay small where the full ones explode.
    fn feasible_t(&mut self, st: State, need: u8, cut: Cut) -> (Vec<Set>, bool) {
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        if need == 0 {
            return (vec![c], true);
        }
        let remaining = (st.ns[0].count_ones().max(st.ns[1].count_ones()) as u8
            + u8::from(st.played > 0))
        .min(st.rounds);
        if need > remaining || st.is_over() {
            return (vec![Set::EMPTY], true);
        }
        match (self.memo_t.get(&(st, need)), cut) {
            (Some((b, v)), Cut::Budget(budget)) if *b >= budget => {
                return (v.clone(), *b == u64::MAX);
            }
            // A smaller budget may have kept nothing worth a beam.
            (Some((u64::MAX, v)), Cut::Beam(_)) => return (v.clone(), true),
            _ => {}
        }
        if let Cut::Beam(_) = cut
            && let Some(v) = self.beam_t.get(&(st, need))
        {
            return (v.clone(), false);
        }
        if self.tick() {
            return (Vec::new(), false);
        }
        let mut w = match cut {
            Cut::Budget(b) if b != u64::MAX => self.post(&st, &s),
            _ => Vec::new(),
        };
        let budget = match cut {
            Cut::Budget(b) if b != u64::MAX && b < Self::deals(&w, c) => b,
            _ => u64::MAX,
        };
        let cut = match cut {
            Cut::Budget(_) => Cut::Budget(budget),
            beam => beam,
        };
        let mut kept = true;
        let out = if Self::is_declarer_turn(&st) {
            let mut all = Vec::new();
            for (ch, _, _) in self.declarer_moves(&st, &s) {
                let (vs, whole) = self.feasible_t(ch, need, cut);
                if self.interrupted {
                    return (Vec::new(), false);
                }
                kept &= whole;
                all.extend(vs);
            }
            let mut out = maximal_sets(all, s.len, None);
            out.retain(|&v| budget == u64::MAX || Self::deals(&w, c - v) <= budget);
            if matches!(cut, Cut::Beam(k) if out.len() > k) && w.is_empty() {
                w = self.post(&st, &s);
            }
            Self::trim(out, cut, &mut kept, |&v| Self::deals(&w, c - v))
        } else {
            // Every vector lies in what each branch's vectors reach, so the
            // merge starts there.
            let mut start = c;
            let mut subs = Vec::new();
            for (_, g, ch, won) in self.defender_moves(&st, &s, c) {
                let sub_need = need - u8::from(won);
                if sub_need == 0 {
                    continue;
                }
                let map = self.map(&st, &ch);
                let (vs, whole) = self.feasible_t(ch, sub_need, cut);
                if self.interrupted {
                    return (Vec::new(), false);
                }
                kept &= whole;
                let sub = maximal_sets(
                    vs.iter()
                        .map(|y| g.iter().filter(|&i| y.contains(map[i])).collect())
                        .filter(|&p| budget == u64::MAX || Self::deals(&w, g - p) <= budget)
                        .collect(),
                    s.len,
                    None,
                );
                start = start - (g - sub.iter().fold(Set::EMPTY, |a, &p| a | p));
                let most = if budget == u64::MAX {
                    0
                } else {
                    sub.iter().map(|&p| Self::deals(&w, p)).max().unwrap_or(0)
                };
                subs.push((g, sub, most));
            }
            // The order changes the work, not the answer; narrow branches
            // first keeps the rows few while the budget bites.
            // ponytail: the budget bites only while a state holds more deals
            // than it, i.e. in the first trick; deeper the search is the
            // full one.  KJ9753 - exact still takes 13 s in the lead's
            // merge.  Giving each branch the budget left after the
            // double-dummy losses off it measured no gain.
            if budget != u64::MAX {
                subs.sort_by_key(|(_, sub, _)| sub.len());
            }
            let mut acc = vec![start];
            for b in 0..subs.len() {
                let (g, ref sub, _) = subs[b];
                // A vector loses at least what it holds in a later branch
                // beyond the most any vector there holds.
                let keep = |&r: &Set| {
                    budget == u64::MAX
                        || Self::deals(&w, c - r)
                            + subs[b + 1..]
                                .iter()
                                .map(|&(h, _, most)| Self::deals(&w, r & h).saturating_sub(most))
                                .max()
                                .unwrap_or(0)
                            <= budget
                };
                let mut next = Vec::new();
                for &x in &acc {
                    // min on the branch's layouts: x loses those y lacks
                    next.extend(maximal_sets(
                        sub.iter().map(|&y| x - (g - y)).filter(keep).collect(),
                        s.len,
                        None,
                    ));
                }
                acc = maximal_sets(next, s.len, Some(&g));
                if matches!(cut, Cut::Beam(k) if acc.len() > k) && w.is_empty() {
                    w = self.post(&st, &s);
                }
                acc = Self::trim(acc, cut, &mut kept, |&v| Self::deals(&w, c - v));
            }
            acc
        };
        match cut {
            Cut::Beam(_) if !kept => {
                self.beam_t.insert((st, need), out.clone());
            }
            _ => {
                self.memo_t.insert((st, need), (budget, out.clone()));
            }
        }
        (out, kept && budget == u64::MAX)
    }

    /// Each trick target's optimal vector from `st`, in tricks still needed
    /// on the layouts it wins, for every target some layout can still reach
    fn adopt(&mut self, st: State, c: Set) -> Vec<Tricks> {
        let mut out = Vec::new();
        for need in 1.. {
            let (score, v) = self.value(st, Goal::Tricks { target: need }, 0);
            if score == 0 {
                break;
            }
            let mut r = Tricks::ZERO;
            for i in c.iter() {
                r.set(i, v.get(i) * need);
            }
            out.push(r);
        }
        out
    }

    /// `req` on the child after a defender's card that keeps the layouts `g`
    /// (lifted by `map`), less the trick if declarer won it
    fn pass(req: &Tricks, g: Set, map: &[usize], won: bool) -> Tricks {
        let mut sub = Tricks::ZERO;
        for i in g.iter() {
            let r = req.get(i).saturating_sub(u8::from(won));
            sub.set(map[i], sub.get(map[i]).max(r));
        }
        sub
    }

    /// Tricks per layout from `st` of the [`Goal::Expected`] line that
    /// follows `req` (tricks still needed per layout, from some target's
    /// optimal vector; zero = none yet).  At each declarer turn it plays,
    /// of the moves that keep `req`, the one of most expected tricks; where
    /// `req` runs out (made, or hopeless), it adopts whichever target's
    /// vector from there gives the most.  Dropping `req` anywhere else is not
    /// an option: a falsecarding defender sends each layout down its worst
    /// branch, so a switch that looks better from one state can lose overall.
    // ponytail: greedy over a few vectors per state, so a lower bound on the
    // best; Goal::ExactExpected is the exact (exponential) search.
    fn line(&mut self, st: State, req: Tricks) -> Tricks {
        if let Some(&(t, _)) = self.memo_line.get(&(st, req)) {
            return t;
        }
        if self.tick() {
            return Tricks::ZERO;
        }
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        let ev = |t: &Tricks| Self::score(&s, c, Goal::Expected, 0, t);
        let (t, step) = if st.played == 0 && st.ns == [0; 2] {
            (Tricks::ZERO, Step::Done)
        } else if req == Tricks::ZERO {
            let mut best = (0, Tricks::ZERO, Step::Done);
            for (j, r) in self.adopt(st, c).into_iter().enumerate() {
                let t = self.line(st, r);
                if ev(&t) > best.0 {
                    best = (ev(&t), t, Step::Adopt(j));
                }
            }
            (best.1, best.2)
        } else if Self::is_declarer_turn(&st) {
            let mut best: Option<(u64, Tricks, Step)> = None;
            for (k, (ch, _, _)) in self.declarer_moves(&st, &s).into_iter().enumerate() {
                if !self.guarantees(ch, &req, s.len, Goal::Expected) {
                    continue;
                }
                let t = self.line(ch, req);
                if best.is_none_or(|(b, _, _)| ev(&t) > b) {
                    best = Some((ev(&t), t, Step::Move(k)));
                }
            }
            if self.interrupted {
                return Tricks::ZERO;
            }
            let (_, t, step) = best.expect("req is feasible, so some move keeps it");
            (t, step)
        } else {
            let mut t = Tricks::ZERO;
            for i in c.iter() {
                t.set(i, 127);
            }
            for (_, g, ch, won) in self.defender_moves(&st, &s, c) {
                let map = self.map(&st, &ch);
                let sub = self.line(ch, Self::pass(&req, g, &map, won));
                for i in g.iter() {
                    t.set(i, t.get(i).min(sub.get(map[i]) + u8::from(won)));
                }
            }
            (t, Step::Branch)
        };
        if !self.interrupted {
            self.memo_line.insert((st, req), (t, step));
        }
        t
    }

    /// The best [`line`](Self::line) from `st`: following one of `reqs`
    /// that `st` still guarantees, or starting afresh.  Returns its tricks
    /// per layout and the requirement followed (zero = afresh).
    fn best_line(&mut self, st: State, reqs: &[Tricks]) -> (Tricks, Tricks) {
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        let mut best: Option<(u64, Tricks, Tricks)> = None;
        for &r in reqs.iter().chain(&[Tricks::ZERO]) {
            if r != Tricks::ZERO && !self.guarantees(st, &r, s.len, Goal::Expected) {
                continue;
            }
            let t = self.line(st, r);
            let ev = Self::score(&s, c, Goal::Expected, 0, &t);
            if best.is_none_or(|(b, _, _)| ev > b) {
                best = Some((ev, t, r));
            }
        }
        let (_, t, r) = best.expect("switching afresh is always an option");
        (t, r)
    }

    /// The [`line`](Self::line) from `st` following `req`, as a [`Plan`]
    fn plan_line(&mut self, st: State, req: Tricks) -> Plan {
        self.line(st, req);
        if self.interrupted {
            return Plan::Hopeless;
        }
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        match self.memo_line[&(st, req)].1 {
            Step::Done => Plan::Made,
            Step::Adopt(j) => {
                let r = self.adopt(st, c)[j];
                self.plan_line(st, r)
            }
            Step::Move(k) => {
                let (ch, at, cards) = self.declarer_moves(&st, &s)[k];
                Plan::Declarer {
                    seat: seat(at),
                    cards: Holding::from_bits_truncate(cards),
                    then: Box::new(self.plan_line(ch, req)),
                }
            }
            Step::Branch => {
                let mut branches = Vec::new();
                for (class, g, ch, won) in self.defender_moves(&st, &s, c) {
                    let map = self.map(&st, &ch);
                    let sub_req = Self::pass(&req, g, &map, won);
                    let sub = if self.line(ch, sub_req) == Tricks::ZERO {
                        Plan::Hopeless
                    } else {
                        self.plan_line(ch, sub_req)
                    };
                    Self::add_branch(&mut branches, &s, class, sub);
                }
                Plan::Defender {
                    seat: seat((st.leader + st.played) % 4),
                    branches,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift64
    fn next(x: &mut u64) -> u64 {
        *x ^= *x << 13;
        *x ^= *x >> 7;
        *x ^= *x << 17;
        *x
    }

    /// `n` random subsets of a few random sets over `len` lanes, so that
    /// many dominate others and some repeat
    fn sets(x: &mut u64, n: usize, len: usize) -> Vec<Set> {
        let bases: Vec<Set> = (0..4)
            .map(|_| (0..len).filter(|_| !next(x).is_multiple_of(4)).collect())
            .collect();
        (0..n)
            .map(|_| {
                let base = bases[next(x) as usize % 4];
                (0..len)
                    .filter(|&i| base.contains(i) && !next(x).is_multiple_of(8))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn sets_reduce_like_tricks() {
        let mut x = 1;
        for len in [3, 9, 64, 65, 108, 256] {
            let vs = sets(&mut x, 300, len);
            let tricks = vs.iter().map(|&v| Solver::ones(v)).collect();
            let got: Vec<Tricks> = maximal_sets(vs, len, None)
                .into_iter()
                .map(Solver::ones)
                .collect();
            assert_eq!(got, maximal(tricks, len, None));
        }
    }

    #[test]
    fn grouping_changes_nothing() {
        let mut x = 7;
        for len in [9, 64, 108, 256] {
            let g: Set = (0..len)
                .filter(|_| !next(&mut x).is_multiple_of(3))
                .collect();
            let acc = sets(&mut x, 20, len);
            let sub = sets(&mut x, 20, len);
            let mut rows: Vec<Set> = sets(&mut x, 50, len);
            for &a in &acc {
                rows.extend(sub.iter().map(|&y| a - (g - y)));
            }
            let plain = maximal_sets(rows.clone(), len, None);
            assert_eq!(maximal_sets(rows, len, Some(&g)), plain);

            let mut mask = Tricks::ZERO;
            for i in g.iter() {
                mask.set(i, 0xFF);
            }
            let lanes = |x: &mut u64| {
                let mut t = Tricks::ZERO;
                for i in 0..len {
                    t.set(i, (next(x) % 4) as u8);
                }
                t
            };
            let acc: Vec<Tricks> = (0..20).map(|_| lanes(&mut x)).collect();
            let sub: Vec<Tricks> = (0..20).map(|_| lanes(&mut x)).collect();
            let mut rows: Vec<Tricks> = (0..50).map(|_| lanes(&mut x)).collect();
            for a in &acc {
                rows.extend(sub.iter().map(|y| a.min_on(y, &mask, len.div_ceil(8))));
            }
            let plain = maximal(rows.clone(), len, None);
            assert_eq!(maximal(rows, len, Some(&mask)), plain);
        }
    }

    /// After each opening lead, for every target and for expected tricks, a
    /// budget keeps exactly the full antichain's vectors within it: searched
    /// afresh, or reusing answers to larger and smaller budgets.  A beam's
    /// vectors lie under the full antichain's.
    #[test]
    fn cuts_keep_what_they_promise() {
        let pairs = [("AQT8", "642"), ("KQT32", "654"), ("A8", "JT62")];
        for (north, south) in pairs {
            let (north, south) = (north.parse().unwrap(), south.parse().unwrap());
            let new = || Solver::new(north, south).unwrap();
            let (mut full, mut reuse, mut beam) = (new(), new(), new());
            let root = full.root();
            let s = full.structure(root.live());
            for (st, _, _) in full.declarer_moves(&root, &s) {
                let c = Solver::consistent(&st, &s);
                let w = full.post(&st, &s);
                let cap = Solver::cap(&st);
                let total = Solver::deals(&w, c);
                let budgets = [0, 1, 2, 4, 6, 3, 1].map(|k| total * k / 8);
                for need in 1..=full.max_tricks() {
                    let (all, kept) = full.feasible_t(st, need, Cut::ALL);
                    assert!(kept);
                    for b in budgets {
                        let within = |vs: Vec<Set>| -> Vec<Set> {
                            vs.into_iter()
                                .filter(|&v| Solver::deals(&w, c - v) <= b)
                                .collect()
                        };
                        let fresh = new().feasible_t(st, need, Cut::Budget(b)).0;
                        assert_eq!(within(fresh), within(all.clone()));
                        let (some, _) = reuse.feasible_t(st, need, Cut::Budget(b));
                        assert_eq!(within(some), within(all.clone()));
                    }
                    let (some, _) = beam.feasible_t(st, need, Cut::Beam(4));
                    assert!(some.iter().all(|v| all.iter().any(|a| a.covers(v, s.len))));
                }
                let (all, _) = full.feasible(st, Cut::ALL);
                for b in budgets.map(|b| b * u64::from(cap)) {
                    let within = |vs: Vec<Tricks>| -> Vec<Tricks> {
                        vs.into_iter()
                            .filter(|v| Solver::lost(&w, c, cap, v) <= b)
                            .collect()
                    };
                    let fresh = new().feasible(st, Cut::Budget(b)).0;
                    assert_eq!(within(fresh), within(all.clone()));
                    let (some, _) = reuse.feasible(st, Cut::Budget(b));
                    assert_eq!(within(some), within(all.clone()));
                }
                let (some, _) = beam.feasible(st, Cut::Beam(4));
                assert!(some.iter().all(|v| all.iter().any(|a| a.covers(v, s.len))));
            }
        }
    }
}
