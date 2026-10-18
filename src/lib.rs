//! Single-dummy suit-combination solver for contract bridge.
//!
//! Declarer (North–South) sees only the cards played; the defenders see
//! everything and, for their actual layout, pick the card that hurts most.
//! [`analyze`] finds the pure declarer strategy that is best for a target:
//! taking at least that many tricks in the suit.  [`Solver::compare`] sets
//! every line against every other at matchpoints or IMPs, as SuitPlay does.
//! Layouts are weighted by vacant places: one giving West `w` of
//! the `n` missing cards has weight `C(26 - n, 13 - w)` out of
//! <code>[TOTAL] = C(26, 13)</code>.  [`Constraints`] set other vacant
//! places, cards known to be with East or West, and ranges of each
//! defender's length and high-card points in the suit; scores are then out
//! of [`Analysis::total`].  They also set where the lead starts, how many
//! entries each hand has outside the suit, how many rounds are played, a
//! fixed first card, declarer's cards discarded elsewhere, and ruffs.
//!
//! The search is exact.  Instead of a greedy per-branch value — which is *not*
//! optimal here, because a falsecarding defender spreads one layout's weight
//! over several branches — each information state carries the Pareto antichain
//! of "tricks per layout" vectors some continuation guarantees, so the
//! recommended line is the true optimum against omniscient defence.
//!
//! That holds for every trick target.  Only the best vector counts in the
//! end, so the search first finds a good line cheaply, then keeps only the
//! vectors that might still beat it: one that already loses more deals than
//! the good line cannot.  A comparison keeps the whole front.
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
//! otherwise, no ruffs unless [`Constraints::ruffs`] allows them (one side
//! only, so no overruff), no inference about the enemy hands beyond
//! [`Constraints`], and only tricks won in this suit count.  The defenders
//! never lead the suit: after winning a round they switch, and declarer
//! comes back by an entry.

use contract_bridge::{Contract, Holding, Rank, Seat};
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// `C(26, 13)`: the denominator of every probability without [`Constraints`]
pub const TOTAL: u64 = 10_400_600;

/// Most layouts supported once interchangeable missing cards are merged
/// (layout sets are 256-bit).  Every holding fits (twelve missing cards
/// make at most 162); only known cards or HCP ranges, which tell missing
/// cards apart, can exceed it.
pub const MAX_LAYOUTS: usize = 256;

/// Most classes of missing cards: each at least doubles the layouts
const MAX_CLASSES: usize = MAX_LAYOUTS.ilog2() as usize;

/// Why a problem cannot be analyzed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// North and South hold a card in common
    Overlap,
    /// More than [`MAX_LAYOUTS`] layouts of the missing cards, or all 13
    /// missing
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
    /// Both sides may ruff, or a [`Game`] has ruffs
    Unsupported,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Overlap => "North and South share a card",
            Self::TooManyMissing => {
                "too many missing cards told apart (more than 256 layouts), or no card held"
            }
            Self::BadTarget => "target must be 1..=longer hand and at most the rounds",
            Self::BadWest => "West holds a card of North or South",
            Self::NotPlayable => "that card cannot be played now",
            Self::BadConstraints => {
                "constraints do not fit the cards: known cards, vacant places, discards or \
                 the first lead"
            }
            Self::NoLayout => "no layout fits the constraints",
            Self::Unsupported => "only one side may ruff, and games take no ruffs",
            Self::Interrupted { bound, total } => {
                let bound = *bound as f64 / *total as f64;
                return write!(f, "search interrupted; the best is at most {bound:.4}");
            }
        })
    }
}

impl std::error::Error for Error {}

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
    /// Declarer ruffs from `seat`, void in the suit (see [`Constraints::ruffs`])
    Ruff { seat: Seat, then: Box<Plan> },
    /// A defender's card decides the continuation; `None` = shows out
    Defender {
        seat: Seat,
        branches: Vec<(Vec<Option<Rank>>, Plan)>,
        /// The continuation if the defender ruffs, when they may
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        ruff: Option<Box<Plan>>,
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
    /// Tricks played for
    pub target: u8,
    /// The weight (out of `total`) of the layouts the optimal line makes
    /// the target on
    pub score: u64,
    pub leads: Vec<Lead>,
    pub plan: Plan,
    /// What the line guarantees on each of [`Solver::layouts`]: the target
    /// where it makes and 0 elsewhere
    pub guarantee: Vec<u8>,
    /// Weight of every layout: [`TOTAL`] without [`Constraints`]
    pub total: u64,
}

/// One layout of the missing cards, as [`Analysis::guarantee`] and
/// [`Line::guarantee`] index them
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Layout {
    /// West holds this many of each group of interchangeable cards (any
    /// of them, as no line tells them apart), East the rest
    pub west: Vec<(Holding, u8)>,
    /// Weight of the deals with this layout, out of the total: 0 if the
    /// [`Constraints`] rule it out
    pub weight: u64,
}

impl Analysis {
    /// Success probability
    #[must_use]
    pub fn value(&self) -> f64 {
        self.score as f64 / self.total as f64
    }
}

/// How [`Solver::compare`] scores one line against another, as SuitPlay does
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", rename_all = "snake_case"))]
pub enum Scoring {
    /// How often a line takes more tricks than the other, whatever the margin
    Matchpoints,
    /// Expected IMPs won, in `contract` (by a `vulnerable` declarer), which
    /// it makes with `need` tricks in this suit: each trick more or less in
    /// the suit is one more or less in all
    Imps {
        contract: Contract,
        vulnerable: bool,
        need: u8,
    },
}

impl Scoring {
    /// What `x` tricks win over `y`, less what they lose
    fn net(self, x: u8, y: u8) -> i64 {
        self.gain(x, y) as i64 - self.gain(y, x) as i64
    }

    /// What line `a`'s `x` tricks in the suit win over line `b`'s `y`:
    /// a deal (matchpoints) or IMPs
    fn gain(self, x: u8, y: u8) -> u64 {
        match self {
            Self::Matchpoints => u64::from(x > y),
            Self::Imps {
                contract,
                vulnerable,
                need,
            } => {
                let made = 6 + contract.bid.level.get() + 13;
                let score =
                    |t: u8| contract.score((made + t - need).clamp(13, 26) - 13, vulnerable);
                imps(score(x) - score(y))
            }
        }
    }
}

/// IMPs for a difference of `points` in favour (0 if against), by the WBF
/// scale
fn imps(points: i32) -> u64 {
    const SCALE: [i32; 24] = [
        20, 50, 90, 130, 170, 220, 270, 320, 370, 430, 500, 600, 750, 900, 1100, 1300, 1500, 1750,
        2000, 2250, 2500, 3000, 3500, 4000,
    ];
    SCALE.iter().filter(|&&t| points >= t).count() as u64
}

/// One line of a [`Comparison`]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Line {
    /// `tricks[k]`: weight of the layouts on which the line takes `k` tricks
    pub tricks: Vec<u64>,
    /// Tricks the line takes on each of [`Solver::layouts`]
    pub guarantee: Vec<u8>,
    /// The first card: one of `cards` led from `seat`
    pub seat: Seat,
    pub cards: Holding,
    /// The line, its first card included: a large tree, so not serialized
    #[cfg_attr(feature = "serde", serde(skip))]
    pub plan: Plan,
}

/// Result of [`Solver::compare`]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Comparison {
    pub scoring: Scoring,
    /// The best line found, then for each other first card the best line
    /// found with it, by what it gains over the first, most first; and any
    /// line found beating the first in a cycle
    pub lines: Vec<Line>,
    /// `payoff[x][y]`: what `x` tricks win over `y` on one deal: 1 or 0 at
    /// matchpoints, IMPs at IMPs
    pub payoff: Vec<Vec<u64>>,
    /// Weight of each of [`Solver::layouts`] that declarer may face: 0 for
    /// the layouts no line plays for
    pub weights: Vec<u64>,
    /// `[0]`, or, if the lines found beat each other in a cycle, the
    /// smallest set of them that beat or tie every listed line outside it
    /// (where `a` beats `b` if [`gain(a, b)`](Self::gain) is more than
    /// `gain(b, a)`)
    pub best: Vec<usize>,
    /// No line at all beats line 0: a search of every line found none
    pub proven: bool,
    /// That search was interrupted; asking again goes on with it
    pub open: bool,
    /// Weight of every layout: [`TOTAL`] without [`Constraints`]
    pub total: u64,
}

impl Comparison {
    /// What line `a` wins over line `b`, summed over layouts by weight:
    /// deals at matchpoints, IMPs times deals at IMPs.  Out of `total`.
    #[must_use]
    pub fn gain(&self, a: usize, b: usize) -> u64 {
        let (x, y) = (&self.lines[a].guarantee, &self.lines[b].guarantee);
        (self.weights.iter().zip(x).zip(y))
            .map(|((w, &x), &y)| w * self.payoff[usize::from(x)][usize::from(y)])
            .sum()
    }

    /// [`best`](Self::best) when nothing is proven
    fn smith(&self) -> Vec<usize> {
        let n = self.lines.len();
        let ge = |a, b| self.gain(a, b) >= self.gain(b, a);
        // Every best line reaches any line; the last standing beats or ties
        // those after it, so few lines reach it
        let c = (1..n).fold(0, |c, x| if ge(c, x) { c } else { x });
        // The lines reaching `c` (the best among them, as nothing outside
        // reaches one of them) by "beats or ties"
        let mut near = vec![c];
        let mut seen = vec![false; n];
        seen[c] = true;
        let mut k = 0;
        while let Some(&y) = near.get(k) {
            for (x, seen) in seen.iter_mut().enumerate() {
                if !*seen && ge(x, y) {
                    *seen = true;
                    near.push(x);
                }
            }
            k += 1;
        }
        near.sort_unstable();
        // Those of them reaching every one of them
        // ponytail: Floyd–Warshall, cubic in the lines reaching `c`: a
        // hundred or so seen; Tarjan's components if they grow to thousands
        let m = near.len();
        let mut reach: Vec<Vec<bool>> = (0..m)
            .map(|a| (0..m).map(|b| ge(near[a], near[b])).collect())
            .collect();
        for k in 0..m {
            let via = reach[k].clone();
            for row in reach.iter_mut().filter(|row| row[k]) {
                for (r, &v) in row.iter_mut().zip(&via) {
                    *r |= v;
                }
            }
        }
        (0..m)
            .filter(|&a| reach[a].iter().all(|&r| r))
            .map(|a| near[a])
            .collect()
    }
}

/// One card declarer may play at a position of a line, against the line's
/// own card: see [`Solver::choices`]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Choice {
    /// One of `cards` (all equivalent) from `seat`; empty: a discard
    pub seat: Seat,
    pub cards: Holding,
    /// The line plays it
    pub line: bool,
    /// What the best line on from this card wins over the line's own way on,
    /// less what it loses, summed by weight over the deals still possible:
    /// deals at matchpoints, IMPs times deals at IMPs, out of
    /// [`Choices::total`]
    pub net: i64,
    /// `net` is the most any way on gains; else what one found gains, as
    /// the search was interrupted
    pub exact: bool,
}

/// Every card declarer may play at a position of a line
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Choices {
    pub choices: Vec<Choice>,
    /// Weight of the deals still possible after the cards played
    pub total: u64,
}

/// What declarer knows of the defenders' hands besides the cards they play
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default, deny_unknown_fields))]
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
    /// Rounds North, East, South, West may each ruff, once void in the
    /// suit.  A ruff is optional, beats every card of the suit, and wins the
    /// trick, which counts as a trick in the suit.  Only one side may ruff:
    /// trump ranks are not modelled, so there is no overruff.
    pub ruffs: [u8; 4],
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
            ruffs: [0; 4],
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

/// Best line for at least `target` tricks with `north` opposite `south`
pub fn analyze(north: Holding, south: Holding, target: u8) -> Result<Analysis, Error> {
    Solver::new(north, south)?.analyze(target)
}

/// [`analyze`] for every target from one trick up to the longer hand's
/// length
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
struct Tricks([u64; MAX_LAYOUTS / 8]);

impl Tricks {
    const ZERO: Self = Self([0; MAX_LAYOUTS / 8]);
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

/// Vectors a merge of [`Solver::banded`] gathers before reducing them
const MERGE_BATCH: usize = 1 << 16;

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
    shown: [[u8; MAX_CLASSES]; 2],
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
    /// Ruffs left to North, East, South, West
    ruffs: [u8; 4],
}

/// Rank of a ruff in [`State::trick`]: above the suit's ace.  A declarer
/// move of class `1 << RUFF` is a ruff.
const RUFF: u8 = 15;

/// What a defender plays
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Card {
    /// A card of this class of the [`Structure`]
    Class(usize),
    /// A discard: the defender shows out
    Out,
    /// A ruff, once void
    Ruff,
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
            let r = self.trick[p];
            if (self.leader + p as u8).is_multiple_of(2) && r != 0 && r != RUFF {
                live |= 1 << r;
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
    /// The classes of the `missing` cards with `live` declarer cards, and
    /// the number of layouts they make
    fn classes(missing: u16, live: u16, c: &Constraints) -> (Vec<Class>, usize) {
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
        (classes, len)
    }

    fn new(missing: u16, live: u16, c: &Constraints) -> Self {
        let [east, west] = c.known.map(Holding::to_bits);
        let (classes, len) = Self::classes(missing, live, c);
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

/// At a defender's turn, a card's layouts as lanes and the ways on after it
type Way = (Tricks, Vec<Tricks>);

/// A search of lines by how they compare with a line
struct Banded {
    scoring: Scoring,
    line: Tricks,
    /// What a vector may lose, against double dummy, and still gain enough
    budget: u64,
    /// Per root layout, the tricks at and below which, and at and above
    /// which, every comparison is the same
    lo: Vec<u8>,
    hi: Vec<u8>,
    /// Per root layout and tricks taken there, what a deal of it loses
    /// against taking every trick
    loss: Vec<Vec<u64>>,
    /// Live cards of the root
    live: u16,
    bands: FxMap<(u16, u8), Rc<Band>>,
    /// Vectors losing at most the budget kept with them, and maybe others
    memo: FxMap<(State, u8), (u64, Vec<Tricks>)>,
}

impl Banded {
    /// A search from a state of structure `live` worth `value[i][x]` on
    /// each layout `i` there of `mass` deals, taking `x` tricks in all
    /// (nondecreasing in `x`); no line takes more than `most(i)`.  A layout
    /// of no deals is left out.  `scoring`, `line` and `budget` name it.
    fn new(
        value: &[Vec<i64>],
        mass: &[u64],
        most: impl Fn(usize) -> u8,
        live: u16,
        (scoring, line, budget): (Scoring, Tricks, u64),
    ) -> Self {
        let (mut lo, mut hi, mut loss) = (Vec::new(), Vec::new(), Vec::new());
        for (i, v) in value.iter().enumerate() {
            let max = v.len() - 1;
            if mass[i] == 0 {
                lo.push(u8::MAX);
                hi.push(0);
                loss.push(vec![u64::MAX; max + 1]);
                continue;
            }
            let most = usize::from(most(i)).min(max);
            // Worth the same whatever is taken: every trick count alike
            let flat = v[0] == v[max];
            lo.push(if flat {
                0
            } else {
                (0..=max).rev().find(|&x| v[x] == v[0]).unwrap_or(0) as u8
            });
            hi.push(if flat {
                0
            } else {
                (0..=max).find(|&x| v[x] == v[max]).unwrap_or(max) as u8
            });
            // Per deal, rounded down: deals at a later state are fewer.
            loss.push(
                (0..=max)
                    .map(|x| (v[most] - v[x.min(most)]) as u64 / mass[i])
                    .collect(),
            );
        }
        Self {
            scoring,
            line,
            budget,
            lo,
            hi,
            loss,
            live,
            bands: FxMap::default(),
            memo: FxMap::default(),
        }
    }
}

/// [`Banded`] at one structure after some tricks
struct Band {
    lo: Tricks,
    hi: Tricks,
    /// Per layout and tricks from here, the least a deal of it loses: of
    /// the root layouts becoming it, the one losing least
    loss: Vec<Vec<u64>>,
}

/// The search for one combination, memoized across targets, comparisons and [`Game`]s
pub struct Solver {
    north: Holding,
    south: Holding,
    missing: u16,
    constraints: Constraints,
    /// Layout spaces by live declarer cards
    structures: FxMap<u16, Rc<Structure>>,
    /// Antichains of tricks per layout, for [`Solver::front`] and the
    /// narrow search of [`Solver::compare`], with the
    /// [`Cut::Budget`] they hold
    memo: FxMap<State, (u64, Vec<Tricks>)>,
    /// Antichains of 0/1 per layout by tricks still needed, for trick
    /// targets, with the [`Cut::Budget`] they hold
    memo_t: FxMap<(State, u8), (u64, Vec<Set>)>,
    /// [`Cut::Beam`] answers that left something out
    beam: FxMap<State, Vec<Tricks>>,
    beam_t: FxMap<(State, u8), Vec<Set>>,
    /// [`Structure::map_to`] by the live cards of parent and child
    maps: FxMap<(u16, u16), Rc<[usize]>>,
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
    /// The last search for lines beating a line, kept to go on with
    banded: Option<Banded>,
}

/// Memo misses between polls of [`Solver::set_interrupt`]'s callback
const POLL_EVERY: u32 = 1024;

impl Solver {
    pub fn new(north: Holding, south: Holding) -> Result<Self, Error> {
        Self::with_constraints(north, south, Constraints::default())
    }

    /// A solver for `north` opposite `south` under `constraints`
    pub fn with_constraints(
        north: Holding,
        south: Holding,
        constraints: Constraints,
    ) -> Result<Self, Error> {
        if !(north & south).is_empty() {
            return Err(Error::Overlap);
        }
        let missing = (Holding::ALL - north - south).to_bits();
        if (north | south).is_empty() {
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
        let [n, e, s, w] = constraints.ruffs;
        if (n > 0 || s > 0) && (e > 0 || w > 0) {
            return Err(Error::Unsupported);
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
            interrupt: None,
            poll_every: POLL_EVERY,
            misses: 0,
            armed: false,
            interrupted: false,
            dd: None,
            banded: None,
        };
        let root = solver.root();
        // The root has the most live cards, so the finest layout space
        if Structure::classes(missing, root.live(), &constraints).1 > MAX_LAYOUTS {
            return Err(Error::TooManyMissing);
        }
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
    // seconds to combine overruns by that much; killing the thread or
    // worker is the hard stop.
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
    /// [`tick`](Self::tick), polling the interrupt now
    fn poll(&mut self) -> bool {
        self.misses = self.poll_every.saturating_sub(1);
        self.tick()
    }

    fn tick(&mut self) -> bool {
        if self.armed && !self.interrupted {
            self.misses += 1;
            if self.misses >= self.poll_every {
                self.misses = 0;
                let states = self.memo.len() + self.memo_t.len();
                self.interrupted = self.interrupt.as_mut().is_some_and(|f| f(states));
            }
        }
        self.interrupted
    }

    /// Diagnostics for benchmarks: memoized states and the largest antichain
    #[doc(hidden)]
    pub fn stats(&self) -> (usize, usize) {
        let states = self.memo.len() + self.memo_t.len();
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
    /// Most tricks a target may ask: the longer hand's cards in play, at
    /// most [`Constraints::rounds`]
    pub fn max_tricks(&self) -> u8 {
        let st = self.root();
        let longer = st.ns[0].count_ones().max(st.ns[1].count_ones()) as u8;
        longer.min(self.constraints.rounds)
    }

    fn check(&self, target: u8) -> Result<(), Error> {
        if target == 0 || target > self.max_tricks() {
            return Err(Error::BadTarget);
        }
        Ok(())
    }

    fn root(&self) -> State {
        let c = &self.constraints;
        let ns = [self.north, self.south].map(|h| (h - c.discards).to_bits());
        let longer = ns[0].count_ones().max(ns[1].count_ones()) as u8;
        let entries = c.entries.map(|e| e.unwrap_or(UNLIMITED));
        let unlimited = u8::from(entries[0] == UNLIMITED) | u8::from(entries[1] == UNLIMITED) << 1;
        State {
            ns,
            shown: [[0; MAX_CLASSES]; 2],
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
            ruffs: c.ruffs,
        }
    }

    /// Some continuation from `st` wins at least `req` tricks per layout;
    /// every nonzero lane of `req` holds the same count on a target's path
    fn guarantees(&mut self, st: State, req: &Tricks, len: usize) -> bool {
        // A vector covering `req` loses no more than `req` does.
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        let need = (0..len).map(|i| req.get(i)).max().unwrap_or(0);
        let req: Set = (0..len).filter(|&i| req.get(i) > 0).collect();
        let budget = match self.memo_t.get(&(st, need)) {
            Some(&(u64::MAX, _)) => u64::MAX,
            _ => Self::deals(&self.post(&st, &s), c - req),
        };
        let (vs, _) = self.feasible_t(st, need, Cut::Budget(budget));
        vs.iter().any(|v| v.covers(&req, len))
    }

    /// The most weight some continuation from `st` makes `target` on after
    /// `won` tricks, with the 0/1 vector that reaches it
    fn value(&mut self, st: State, target: u8, won: u8) -> (u64, Tricks) {
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        // A beam finds some line's score `x` (or the whole antichain).  The
        // best line scores no less, so it falls short of `all` by at most
        // `all - x`, which bounds the deals it loses (see `budget`).  The
        // search keeping every vector within that loss finds it, and what
        // else it returns scores below `x`.
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

    /// [`Analysis`] for every target from one trick up to the longer hand's
    /// length; the first one interrupted ends it
    pub fn analyze_all(&mut self) -> Result<Vec<Analysis>, Error> {
        (1..=self.max_tricks())
            .map(|target| self.analyze(target))
            .collect()
    }

    /// Best line for at least `target` tricks, or [`Error::Interrupted`] if
    /// the interrupt (see [`set_interrupt`](Self::set_interrupt)) fires first
    pub fn analyze(&mut self, target: u8) -> Result<Analysis, Error> {
        self.check(target)?;
        self.armed = self.interrupt.is_some();
        self.misses = 0;
        let a = self.search(target);
        self.armed = false;
        if std::mem::take(&mut self.interrupted) {
            return Err(Error::Interrupted {
                bound: self.ceiling(target),
                total: self.total(),
            });
        }
        a
    }

    /// [`analyze`](Self::analyze) without the bookkeeping of interrupts
    fn search(&mut self, target: u8) -> Result<Analysis, Error> {
        let root = self.root();
        let structure = self.structure(root.live());
        let moves = self.declarer_moves(&root, &structure);
        let mut leads = Vec::new();
        let mut best: Option<(u64, Tricks, usize)> = None;
        for (i, &(st, at, cards)) in moves.iter().enumerate() {
            let (score, v) = self.value(st, target, 0);
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
        for i in 0..structure.len {
            req.set(i, req.get(i) * target);
        }
        let guarantee = (0..structure.len).map(|i| req.get(i)).collect();
        let then = self.plan(moves[i].0, req);
        let plan = Plan::Declarer {
            seat: leads[i].seat,
            cards: leads[i].cards,
            then: Box::new(then),
        };
        Ok(Analysis {
            north: self.north,
            south: self.south,
            target,
            score,
            leads,
            plan,
            guarantee,
            total: structure.weight.iter().sum(),
        })
    }

    /// Every layout of the missing cards, in the order of
    /// [`Analysis::guarantee`] and [`Line::guarantee`]
    pub fn layouts(&mut self) -> Vec<Layout> {
        let s = self.structure(self.root().live());
        (0..s.len)
            .map(|i| Layout {
                west: s
                    .classes
                    .iter()
                    .map(|c| {
                        let n = i / c.stride % (usize::from(c.size) + 1);
                        (Holding::from_bits_truncate(c.ranks), n as u8)
                    })
                    .collect(),
                weight: s.weight[i],
            })
            .collect()
    }

    /// Every line on the Pareto front of tricks per layout, as tricks per
    /// root layout: the full search, which can take hours, for tests
    #[doc(hidden)]
    pub fn front(&mut self) -> Vec<Vec<u8>> {
        let root = self.root();
        let s = self.structure(root.live());
        let (vs, _) = self.feasible(root, Cut::ALL);
        vs.iter()
            .map(|v| (0..s.len).map(|i| v.get(i)).collect())
            .collect()
    }

    /// The best line under `scoring` (SuitPlay's comparison of lines),
    /// with the best found for each other first card, or
    /// [`Error::Interrupted`] if the interrupt fires before any line is
    /// found.  Once one is, an interrupt leaves [`Comparison::open`].
    ///
    /// The lines found by a narrow search after each first card start it;
    /// the one beating or tying most of them is checked against every
    /// line, and replaced by what beats it most, until none does.
    pub fn compare(&mut self, scoring: Scoring) -> Result<Comparison, Error> {
        self.armed = self.interrupt.is_some();
        self.misses = 0;
        let c = self.compare_lines(scoring);
        self.armed = false;
        self.interrupted = false;
        c
    }

    fn compare_lines(&mut self, scoring: Scoring) -> Result<Comparison, Error> {
        // ponytail: a cycle longer than this is left unfinished
        const ROUNDS: usize = 8;
        let root = self.root();
        let s = self.structure(root.live());
        let c = Self::consistent(&root, &s);
        let weights: Vec<u64> = (0..s.len)
            .map(|i| if c.contains(i) { s.weight[i] } else { 0 })
            .collect();
        let net = |a: &Tricks, b: &Tricks| -> i64 {
            c.iter()
                .map(|i| weights[i] as i64 * scoring.net(a.get(i), b.get(i)))
                .sum()
        };
        let moves = self.declarer_moves(&root, &s);
        // (first move, tricks per layout, plan once built)
        let mut found: Vec<(usize, Tricks, Option<Plan>)> = Vec::new();
        for (k, &(ch, _, _)) in moves.iter().enumerate() {
            let (vs, _) = self.feasible(ch, Cut::Beam(BEAM));
            if self.interrupted {
                return Err(Error::Interrupted {
                    bound: self.total(),
                    total: self.total(),
                });
            }
            found.extend(vs.into_iter().map(|v| (k, v, None)));
        }
        let mut top = (0..found.len())
            .max_by_key(|&a| {
                let wins = found.iter().filter(|b| net(&found[a].1, &b.1) >= 0);
                (wins.count(), std::cmp::Reverse(a))
            })
            .ok_or(Error::BadTarget)?;
        let (mut proven, mut open) = (false, false);
        let mut tops = vec![top];
        for _ in 0..ROUNDS {
            let Ok(beater) = self.beater(scoring, found[top].1) else {
                open = true;
                break;
            };
            let Some((k, z)) = beater else {
                proven = true;
                break;
            };
            let (ch, at, cards) = moves[k];
            let mut b = self.banded.take();
            let (then, v) = self.realize(b.as_mut(), ch, 0, z);
            self.banded = b;
            if self.interrupted {
                open = true;
                break;
            }
            let plan = Self::declarer_step(at, cards, then);
            top = match found.iter().position(|f| f.1 == v) {
                Some(i) => i,
                None => {
                    found.push((k, v, Some(plan)));
                    found.len() - 1
                }
            };
            if tops.contains(&top) {
                break;
            }
            tops.push(top);
        }
        // The narrow search's lines are memoized: planning them needs no budget.
        self.armed = false;
        self.interrupted = false;
        // The best, any line it was beaten by, and the best of each other
        // first card against it
        let mut shown = vec![top];
        shown.extend(tops.iter().filter(|&&t| t != top));
        for k in 0..moves.len() {
            if shown.iter().any(|&i| found[i].0 == k) {
                continue;
            }
            let with = (0..found.len()).filter(|&i| found[i].0 == k);
            if let Some(i) = with.max_by_key(|&i| net(&found[i].1, &found[top].1)) {
                shown.push(i);
            }
        }
        shown[1..].sort_by_key(|&i| std::cmp::Reverse(net(&found[i].1, &found[top].1)));
        let max = self.max_tricks();
        let mut lines = Vec::new();
        for i in shown {
            let (k, v, plan) = &mut found[i];
            let (ch, at, cards) = moves[*k];
            let (plan, v) = match plan.take() {
                Some(plan) => (plan, *v),
                None => {
                    let (then, v) = self.realize(None, ch, 0, *v);
                    (Self::declarer_step(at, cards, then), v)
                }
            };
            let mut tricks = vec![0; usize::from(max) + 1];
            for i in c.iter() {
                tricks[usize::from(v.get(i))] += s.weight[i];
            }
            lines.push(Line {
                tricks,
                guarantee: (0..s.len).map(|i| v.get(i)).collect(),
                seat: seat(at),
                cards: Holding::from_bits_truncate(cards),
                plan,
            });
        }
        let mut cmp = Comparison {
            scoring,
            lines,
            payoff: (0..=max)
                .map(|x| (0..=max).map(|y| scoring.gain(x, y)).collect())
                .collect(),
            weights,
            best: vec![0],
            proven,
            open,
            total: s.weight.iter().sum(),
        };
        if !proven && !open {
            cmp.best = cmp.smith();
        }
        Ok(cmp)
    }

    /// Every card declarer may play once `plays` (each card in order, `None`
    /// for a discard or showing out) are played as `plan` plays them, each
    /// against `plan`'s own card under `scoring`: what `plan` with the best
    /// way on from that card in place of its own gains over `plan`, out of
    /// every deal.  Elsewhere the defenders still play as they would
    /// against `plan`.  An interrupt leaves the rest of the cards with what
    /// a narrow search finds.  [`Error::NotPlayable`] if `plays` leave the
    /// plan or end where declarer has no card of it to play; not with
    /// ruffs.
    pub fn choices(
        &mut self,
        scoring: Scoring,
        plan: &Plan,
        plays: &[Option<Rank>],
    ) -> Result<Choices, Error> {
        if self.constraints.ruffs != [0; 4] {
            return Err(Error::Unsupported);
        }
        let (st, won, node, reach) = self.replay(plan, plays).ok_or(Error::NotPlayable)?;
        let mine = match node {
            Plan::Declarer { seat, cards, .. } => (*seat, *cards),
            _ => return Err(Error::NotPlayable),
        };
        let root = self.root();
        let rs = self.structure(root.live());
        // The plan's tricks per root layout, and the least of them off this
        // position, where the defenders may still go
        let line = self.follow(root, plan, None);
        let off = self.follow(root, plan, Some(plays));
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        let max = self.max_tricks();
        let mut value = vec![vec![0; usize::from(max) + 1]; s.len];
        let mut mass = vec![0; s.len];
        for (r, &j) in reach.iter().enumerate() {
            let Some(j) = j else { continue };
            let w = rs.weight[r];
            mass[j] += w;
            for (x, v) in value[j].iter_mut().enumerate() {
                let x = (x as u8).min(off.get(r));
                *v += w as i64 * scoring.net(x, line.get(r));
            }
        }
        // Per layout and tricks from here
        let ahead: Vec<Vec<i64>> = value
            .iter()
            .map(|v| v[usize::from(won)..].to_vec())
            .collect();
        let net = |v: &Tricks| -> i64 { c.iter().map(|i| ahead[i][usize::from(v.get(i))]).sum() };
        let most = won + Self::cap(&st);
        let top: i64 = c.iter().map(|i| value[i][usize::from(most)]).sum();
        // A narrow search's best, and the line's own way on, bound each card
        // below, before any interrupt.
        let moves = self.declarer_moves(&st, &s);
        let found: Vec<i64> = (moves.iter())
            .map(|&(ch, at, cards)| {
                let ours = (seat(at), Holding::from_bits_truncate(cards)) == mine;
                let (vs, _) = self.feasible(ch, Cut::Beam(BEAM));
                (vs.iter().map(net).chain(ours.then_some(0)))
                    .max()
                    .unwrap_or(i64::MIN)
            })
            .collect();
        self.armed = self.interrupt.is_some();
        self.misses = 0;
        let mut b = Banded::new(&value, &mass, |_| most, st.live(), (scoring, line, 0));
        let mut choices = Vec::new();
        for (&(ch, at, cards), &found) in moves.iter().zip(&found) {
            let mut best = found;
            let mut exact = false;
            if !self.interrupted {
                let budget = u64::try_from(top - best).unwrap_or(0);
                // Clamped tricks are worth what the tricks are.
                self.deepest_from(&mut b, ch, won, budget, &ahead, &mut best);
                exact = !self.interrupted;
            }
            choices.push(Choice {
                seat: seat(at),
                cards: Holding::from_bits_truncate(cards),
                line: (seat(at), Holding::from_bits_truncate(cards)) == mine,
                net: best,
                exact,
            });
        }
        self.armed = false;
        self.interrupted = false;
        Ok(Choices {
            choices,
            total: rs.weight.iter().sum(),
        })
    }

    /// The state after `plays` along `plan`, the tricks won, the rest of
    /// the plan, and the layout there of each root layout still possible;
    /// `None` if `plays` leave the plan
    fn replay<'p>(
        &mut self,
        plan: &'p Plan,
        plays: &[Option<Rank>],
    ) -> Option<(State, u8, &'p Plan, Vec<Option<usize>>)> {
        let (mut st, mut won, mut node) = (self.root(), 0, plan);
        let rs = self.structure(st.live());
        let c = Self::consistent(&st, &rs);
        let mut reach: Vec<Option<usize>> =
            (0..rs.len).map(|r| c.contains(r).then_some(r)).collect();
        for &card in plays {
            let s = self.structure(st.live());
            match node {
                Plan::Declarer {
                    seat: at,
                    cards,
                    then,
                } => {
                    if card.map_or(!cards.is_empty(), |r| !cards.contains(r)) {
                        return None;
                    }
                    let (ch, _, _) = (self.declarer_moves(&st, &s).into_iter())
                        .find(|&(_, a, m)| seat(a) == *at && m == cards.to_bits())?;
                    st = ch;
                    node = then;
                }
                Plan::Defender { branches, .. } => {
                    let c = Self::consistent(&st, &s);
                    let (_, g, ch, w) = (self.defender_moves(&st, &s, c).into_iter())
                        .find(|&(k, _, _, _)| Self::is(&s, k, card))?;
                    let map = self.map(&st, &ch);
                    for j in &mut reach {
                        *j = j.filter(|&i| g.contains(i)).map(|i| map[i]);
                    }
                    won += u8::from(w);
                    st = ch;
                    node = &branches
                        .iter()
                        .find(|(labels, _)| labels.contains(&card))?
                        .1;
                }
                _ => return None,
            }
        }
        Some((st, won, node, reach))
    }

    /// A defender's `card` of `s` is the card played
    fn is(s: &Structure, card: Card, played: Option<Rank>) -> bool {
        match (card, played) {
            (Card::Class(k), Some(r)) => s.classes[k].ranks >> r.get() & 1 != 0,
            (Card::Out, None) => true,
            _ => false,
        }
    }

    /// Tricks `plan` takes per layout of `st` from there on; past `skip`
    /// (cards played from `st`, if any), as many as there are layouts
    /// reach it: that way is left to the defenders' choice elsewhere
    fn follow(&mut self, st: State, plan: &Plan, skip: Option<&[Option<Rank>]>) -> Tricks {
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        if skip.is_some_and(<[_]>::is_empty) {
            let mut all = Tricks::ZERO;
            for i in c.iter() {
                all.set(i, 127);
            }
            return all;
        }
        // The rest of `skip` past `card`, if `card` is its next
        let past = |card: &dyn Fn(Option<Rank>) -> bool| {
            skip.and_then(<[_]>::split_first)
                .filter(|(p, _)| card(**p))
                .map(|(_, rest)| rest)
        };
        match plan {
            Plan::Made | Plan::Hopeless => Tricks::ZERO,
            Plan::Declarer {
                seat: at,
                cards,
                then,
            } => {
                let moves = self.declarer_moves(&st, &s);
                let &(ch, _, _) = (moves.iter())
                    .find(|&&(_, a, m)| seat(a) == *at && m == cards.to_bits())
                    .expect("a plan's card is a move");
                let skip = past(&|p| p.map_or(cards.is_empty(), |r| cards.contains(r)));
                self.follow(ch, then, skip)
            }
            Plan::Ruff { seat: at, then } => {
                let moves = self.declarer_moves(&st, &s);
                let &(ch, _, _) = (moves.iter())
                    .find(|&&(_, a, m)| seat(a) == *at && m == 1 << RUFF)
                    .expect("a plan's ruff is a move");
                self.follow(ch, then, None)
            }
            Plan::Defender { branches, ruff, .. } => {
                let mut out = Tricks::ZERO;
                for i in c.iter() {
                    out.set(i, 127);
                }
                for (card, g, ch, w) in self.defender_moves(&st, &s, c) {
                    let sub = match card {
                        Card::Class(k) => {
                            let label = Some(Rank::new(lowest(s.classes[k].ranks)));
                            &(branches.iter())
                                .find(|(labels, _)| labels.contains(&label))
                                .expect("a plan answers every card")
                                .1
                        }
                        Card::Out => {
                            &(branches.iter())
                                .find(|(labels, _)| labels.contains(&None))
                                .expect("a plan answers every card")
                                .1
                        }
                        Card::Ruff => ruff.as_deref().expect("a plan answers a ruff"),
                    };
                    let skip = past(&|p| Self::is(&s, card, p));
                    let v = self.follow(ch, sub, skip);
                    let map = self.map(&st, &ch);
                    for i in g.iter() {
                        out.set(i, out.get(i).min(v.get(map[i]) + u8::from(w)));
                    }
                }
                out
            }
        }
    }

    /// The first move and the way of comparing with `line` (tricks per root
    /// layout, clamped as [`banded`](Self::banded) does) of the lines
    /// beating it by most under `scoring`, if any does; `Err` if
    /// interrupted.  The search is kept in [`banded`](Self::banded), to go
    /// on with if asked again and for [`realize`](Self::realize).
    fn beater(&mut self, scoring: Scoring, line: Tricks) -> Result<Option<(usize, Tricks)>, ()> {
        let root = self.root();
        let s = self.structure(root.live());
        let c = Self::consistent(&root, &s);
        let max = self.max_tricks();
        // No line takes more on a layout than double dummy.
        let dd = self.double_dummy();
        let most = |r: usize| dd[r].min(max);
        let top: i64 = c
            .iter()
            .map(|r| s.weight[r] as i64 * scoring.net(most(r), line.get(r)))
            .sum();
        if top <= 0 {
            return Ok(None);
        }
        let budget = (top - 1) as u64;
        // Per layout and tricks, what the layout adds to the gain
        let value: Vec<Vec<i64>> = (0..s.len)
            .map(|r| {
                let w = if c.contains(r) { s.weight[r] as i64 } else { 0 };
                (0..=max).map(|x| w * scoring.net(x, line.get(r))).collect()
            })
            .collect();
        let mut b = match self.banded.take() {
            Some(b) if b.scoring == scoring && b.line == line && b.budget == budget => b,
            _ => {
                let mass: Vec<u64> = (0..s.len)
                    .map(|r| if c.contains(r) { s.weight[r] } else { 0 })
                    .collect();
                Banded::new(&value, &mass, most, root.live(), (scoring, line, budget))
            }
        };
        // A lead keeps the root's layouts: its search is the root's.
        let mut best = None;
        let mut floor = 0;
        for (k, (ch, _, _)) in self.declarer_moves(&root, &s).into_iter().enumerate() {
            if let Some(z) = self.deepest_from(&mut b, ch, 0, budget, &value, &mut floor) {
                best = Some((k, z));
            }
            if self.interrupted {
                break;
            }
        }
        self.banded = Some(b);
        if self.interrupted {
            return Err(());
        }
        Ok(best)
    }

    /// The way on from `st` in `b`'s search adding up most `value` (per
    /// layout and tricks from `st`, rising with them), if more than
    /// `floor`, which it raises to that.  At a defender's turn the merge is
    /// searched [`deepest`](Self::deepest) first; elsewhere it takes
    /// [`banded`](Self::banded)'s front.
    fn deepest_from(
        &mut self,
        b: &mut Banded,
        st: State,
        won: u8,
        budget: u64,
        value: &[Vec<i64>],
        floor: &mut i64,
    ) -> Option<Tricks> {
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        if st.is_over() || Self::is_declarer_turn(&st) {
            let front = self.banded(b, st, won, budget);
            let gain = |v: &Tricks| -> i64 {
                c.iter()
                    .map(|r| value[r][usize::from(v.get(r)).min(value[r].len() - 1)])
                    .sum()
            };
            let (g, z) = (front.into_iter())
                .map(|z| (gain(&z), z))
                .max_by_key(|&(g, _)| g)?;
            return (g > *floor).then(|| {
                *floor = g;
                z
            });
        }
        let (start, mut subs) = self.subs(b, &st, won, budget)?;
        subs.sort_by_key(|(_, sub)| sub.len());
        let mut own = vec![(Tricks::ZERO, Vec::new()); subs.len()];
        for r in c.iter() {
            if let Some(j) = subs.iter().rposition(|(m, _)| m.get(r) != 0) {
                own[j].0.set(r, 0xFF);
                own[j].1.push(r);
            }
        }
        let mut found = None;
        let mut drops = FxMap::default();
        self.deepest(
            &subs, &own, start, s.len, value, floor, &mut found, &mut drops,
        );
        found
    }

    /// Depth first through a defender's merge of `subs` from `x`: each
    /// vector of a card's way on in turn, cut once the merge can gain no
    /// more than `floor` (`gain` adds `value` over the lanes, rising with
    /// each).  Raises `floor` to, and keeps in `found`, the best gain past
    /// it.  `own[k]` are the lanes no card after `k` lowers, as a mask and
    /// a list; `drops` keeps each card's least drop on them per merge so far.
    #[allow(clippy::too_many_arguments)]
    fn deepest(
        &mut self,
        subs: &[Way],
        own: &[(Tricks, Vec<usize>)],
        x: Tricks,
        len: usize,
        value: &[Vec<i64>],
        floor: &mut i64,
        found: &mut Option<Tricks>,
        drops: &mut FxMap<(usize, Tricks), i64>,
    ) {
        let val = |t: u8, r: usize| value[r][usize::from(t).min(value[r].len() - 1)];
        let gain = |v: &Tricks| -> i64 { (0..len).map(|r| val(v.get(r), r)).sum() };
        let Some(((mask, sub), rest)) = subs.split_first() else {
            *floor = gain(&x);
            *found = Some(x);
            return;
        };
        if self.tick() {
            return;
        }
        // Each card to come lowers its own lanes by at least its least
        // drop there, and no two cards own a lane.
        // The least drop depends on `x` only on the lanes owned.
        let mut most = gain(&x);
        for (k, ((_, sub), (mask, own))) in subs.iter().zip(own).enumerate() {
            let mut key = x;
            for (a, m) in key.0.iter_mut().zip(mask.0) {
                *a &= m;
            }
            let least = *drops.entry((subs.len() - k, key)).or_insert_with(|| {
                let drop = |y: &Tricks| -> i64 {
                    own.iter()
                        .map(|&r| val(x.get(r), r) - val(x.get(r).min(y.get(r)), r))
                        .sum()
                };
                sub.iter().map(drop).min().unwrap_or(0)
            });
            most -= least;
            if most <= *floor {
                return;
            }
        }
        let words = len.div_ceil(8);
        let next: Vec<Tricks> = (sub.iter())
            .map(|y| x.min_on(y, mask, words))
            .filter(|z| gain(z) > *floor)
            .collect();
        // Capped by `x`, one way on often covers another: drop the other.
        let mut next: Vec<(i64, Tricks)> = (maximal(next, len, None).into_iter())
            .map(|z| (gain(&z), z))
            .collect();
        next.sort_by_key(|n| std::cmp::Reverse(n.0));
        for (g, z) in next {
            if g <= *floor || self.interrupted {
                break;
            }
            self.deepest(rest, &own[1..], z, len, value, floor, found, drops);
        }
    }

    /// A plan from `st`, after `won` tricks, taking at least `req` per
    /// layout as `b`'s search says some line does (clamped there), or else
    /// as the narrow search does, and what the plan takes per layout from
    /// here.  Where `b` asks nothing more, the narrow search's line taking
    /// the most tricks goes on.
    fn realize(
        &mut self,
        mut b: Option<&mut Banded>,
        st: State,
        won: u8,
        req: Tricks,
    ) -> (Plan, Tricks) {
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        let mut req = req;
        if let Some(b) = b.as_deref_mut() {
            // At or below the band, every line compares alike.
            let band = self.band(b, &st, &s, won);
            for i in c.iter() {
                if req.get(i) <= band.lo.get(i) {
                    req.set(i, 0);
                }
            }
        }
        if (0..s.len).all(|i| req.get(i) == 0) {
            if b.is_none() || st.is_over() {
                return (Plan::Made, Tricks::ZERO);
            }
            let (vs, _) = self.feasible(st, Cut::Beam(BEAM));
            let v = vs
                .into_iter()
                .max_by_key(|v| v.sum(s.len))
                .unwrap_or(Tricks::ZERO);
            return self.realize(None, st, won, v);
        }
        if Self::is_declarer_turn(&st) {
            for (ch, at, cards) in self.declarer_moves(&st, &s) {
                let front = match b.as_deref_mut() {
                    Some(b) => self.banded(b, ch, won, b.budget),
                    None => self.feasible(ch, Cut::Beam(BEAM)).0,
                };
                if front.iter().any(|v| v.covers(&req, s.len)) {
                    let (then, v) = self.realize(b, ch, won, req);
                    return (Self::declarer_step(at, cards, then), v);
                }
            }
            assert!(self.interrupted, "req is feasible, so some move keeps it");
            return (Plan::Hopeless, Tricks::ZERO);
        }
        let seat_index = (st.leader + st.played) % 4;
        let mut branches: Vec<(Vec<Option<Rank>>, Plan)> = Vec::new();
        let mut ruff = None;
        let mut out = Tricks::ZERO;
        for i in c.iter() {
            out.set(i, 127);
        }
        for (card, g, ch, w) in self.defender_moves(&st, &s, c) {
            let map = self.map(&st, &ch);
            let bonus = u8::from(w);
            let mut sub_req = Tricks::ZERO;
            for i in g.iter() {
                let r = req.get(i).saturating_sub(bonus);
                if req.get(i) > 0 && r > sub_req.get(map[i]) {
                    sub_req.set(map[i], r);
                }
            }
            let (sub, v) = self.realize(b.as_deref_mut(), ch, won + bonus, sub_req);
            for i in g.iter() {
                out.set(i, out.get(i).min(v.get(map[i]) + bonus));
            }
            Self::add_branch(&mut branches, &mut ruff, &s, card, sub);
        }
        let plan = Plan::Defender {
            seat: seat(seat_index),
            branches,
            ruff,
        };
        (plan, out)
    }

    /// Upper bound on the best score for `target` from what an interrupted
    /// search has finished: its score if its search is done, else its
    /// double-dummy score
    fn ceiling(&mut self, target: u8) -> u64 {
        let root = self.root();
        if self.memo_t.contains_key(&(root, target)) {
            return self.value(root, target, 0).0;
        }
        let s = self.structure(root.live());
        let dd = self.double_dummy();
        (0..s.len)
            .filter(|&i| dd[i] >= target)
            .map(|i| s.weight[i])
            .sum()
    }

    /// Tricks declarer takes on each root layout seeing all four hands,
    /// which no line beats.  One representative deal per layout: missing
    /// cards of a class are interchangeable.  Declarer's ruffs are not
    /// modelled there, so with them the bound is every trick.
    fn double_dummy(&mut self) -> Rc<[u8]> {
        if let Some(dd) = &self.dd {
            return Rc::clone(dd);
        }
        let s = self.structure(self.root().live());
        let [n, _, south, _] = self.constraints.ruffs;
        if n > 0 || south > 0 {
            let dd: Rc<[u8]> = vec![self.max_tricks(); s.len].into();
            self.dd = Some(Rc::clone(&dd));
            return dd;
        }
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
            st.shown = [[0; MAX_CLASSES]; 2];
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
        if rank == RUFF {
            ch.ruffs[usize::from(at)] -= 1;
        } else {
            ch.ns[at as usize / 2] &= !(1 << rank);
        }
        self.advance(ch, s, rank).0
    }

    /// Declarer's options now: (resulting state, seat, equivalence class
    /// played).  Between tricks that includes the choice of hand, among
    /// those [`State::may_lead`]; at third hand a void hand may "discard"
    /// (class 0), or ruff (class `1 << RUFF`) if it has a ruff left.
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
                    if st.ruffs[usize::from(at)] > 0 {
                        out.push((self.declarer_play(st, s, at, RUFF), at, 1 << RUFF));
                    }
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

    /// A defender's options now: (card played, layouts where it is legal,
    /// resulting state, declarer won the trick)
    fn defender_moves(
        &mut self,
        st: &State,
        s: &Structure,
        c: Set,
    ) -> Vec<(Card, Set, State, bool)> {
        let at = (st.leader + st.played) % 4;
        let d = usize::from(at / 2); // 0 = East, 1 = West
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
                out.push((Card::Class(k), g, ch, won));
            }
        }
        if !void.is_empty() {
            let mut ch = *st;
            ch.void[d] = true;
            let (out_ch, won) = self.advance(ch, s, 0);
            out.push((Card::Out, void, out_ch, won));
            if st.ruffs[usize::from(at)] > 0 {
                ch.ruffs[usize::from(at)] -= 1;
                let (ch, won) = self.advance(ch, s, RUFF);
                out.push((Card::Ruff, void, ch, won));
            }
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

    /// Per layout of `st`, after `won` tricks, the tricks from here
    /// between which comparisons in `b` change, and what each loses
    fn band(&mut self, b: &mut Banded, st: &State, s: &Structure, won: u8) -> Rc<Band> {
        let key = (st.live(), won);
        if let Some(band) = b.bands.get(&key) {
            return Rc::clone(band);
        }
        let map: Rc<[usize]> = match self.maps.get(&(b.live, key.0)) {
            Some(map) => Rc::clone(map),
            None => {
                let map: Rc<[usize]> = self.structure(b.live).map_to(s).into();
                self.maps.insert((b.live, key.0), Rc::clone(&map));
                map
            }
        };
        let cap = Self::cap(st);
        let (mut lo, mut hi) = (vec![u8::MAX; s.len], vec![0; s.len]);
        let mut loss = vec![vec![u64::MAX; usize::from(cap) + 1]; s.len];
        for (r, &j) in map.iter().enumerate() {
            lo[j] = lo[j].min(b.lo[r]);
            hi[j] = hi[j].max(b.hi[r]);
            for (v, l) in loss[j].iter_mut().enumerate() {
                let x = (usize::from(won) + v).min(b.loss[r].len() - 1);
                *l = (*l).min(b.loss[r][x]);
            }
        }
        let (mut l, mut h) = (Tricks::ZERO, Tricks::ZERO);
        for j in 0..s.len {
            if lo[j] != u8::MAX {
                l.set(j, lo[j].saturating_sub(won).min(cap));
                h.set(j, hi[j].saturating_sub(won).min(cap));
            } else {
                loss[j].fill(0);
            }
        }
        let band = Rc::new(Band { lo: l, hi: h, loss });
        b.bands.insert(key, Rc::clone(&band));
        band
    }

    /// [`feasible`](Self::feasible) with tricks clamped per layout to the
    /// [`band`](Self::band) of `b`: every way a line can compare with the
    /// lines of `b`, after `won` tricks
    fn banded(&mut self, b: &mut Banded, st: State, won: u8, budget: u64) -> Vec<Tricks> {
        if let Some((kept, v)) = b.memo.get(&(st, won))
            && *kept >= budget
        {
            return v.clone();
        }
        if self.tick() {
            return Vec::new();
        }
        let s = self.structure(st.live());
        let c = Self::consistent(&st, &s);
        let band = self.band(b, &st, &s, won);
        let clamp = |v: &mut Tricks, i: usize| {
            v.set(i, v.get(i).clamp(band.lo.get(i), band.hi.get(i)));
        };
        let w = self.post(&st, &s);
        // At least what a vector loses at the root, however play got here
        let lost = |v: &Tricks| -> u64 {
            c.iter()
                .map(|i| {
                    w[i] * band.loss[i][usize::from(v.get(i).min(127)).min(band.loss[i].len() - 1)]
                })
                .sum()
        };
        let out = if st.is_over() {
            let mut v = Tricks::ZERO;
            for i in c.iter() {
                clamp(&mut v, i);
            }
            vec![v]
        } else if Self::is_declarer_turn(&st) {
            let mut all = Vec::new();
            for (ch, _, _) in self.declarer_moves(&st, &s) {
                all.extend(self.banded(b, ch, won, budget));
                if self.interrupted {
                    return Vec::new();
                }
            }
            let mut out = maximal(all, s.len, None);
            out.retain(|v| lost(v) <= budget);
            out
        } else {
            let Some((start, subs)) = self.subs(b, &st, won, budget) else {
                return Vec::new();
            };
            let words = s.len.div_ceil(8);
            let mut acc = vec![start];
            for (mask, sub) in &subs {
                let (mut next, mut kept) = (Vec::new(), 0);
                for x in &acc {
                    next.extend(maximal(
                        sub.iter().map(|y| x.min_on(y, mask, words)).collect(),
                        s.len,
                        None,
                    ));
                    // One merge can take minutes: reduce as it doubles,
                    // and let the interrupt in each time
                    // ponytail: a reduction itself is not interrupted (8 s
                    // on KJ9753 opposite a void's first trick before that
                    // went depth first); poll inside `undominated` if that
                    // matters
                    if next.len() > 2 * kept + MERGE_BATCH {
                        if self.poll() {
                            return Vec::new();
                        }
                        next = maximal(next, s.len, Some(mask));
                        kept = next.len();
                    }
                }
                acc = maximal(next, s.len, Some(mask));
                // Lanes still to merge hold at least their final tricks.
                acc.retain(|v| lost(v) <= budget);
            }
            acc
        };
        b.memo.insert((st, won), (budget, out.clone()));
        out
    }

    /// At a defender's turn in `b`'s search, what its merge starts from
    /// (per layout, the least over its cards of the most the way on takes)
    /// and per card a [`Way`]; `None` if interrupted
    fn subs(
        &mut self,
        b: &mut Banded,
        st: &State,
        won: u8,
        budget: u64,
    ) -> Option<(Tricks, Vec<Way>)> {
        let s = self.structure(st.live());
        let c = Self::consistent(st, &s);
        let band = self.band(b, st, &s, won);
        let clamp = |v: &mut Tricks, i: usize| {
            v.set(i, v.get(i).clamp(band.lo.get(i), band.hi.get(i)));
        };
        let mut start = Tricks::ZERO;
        for i in c.iter() {
            start.set(i, 127);
        }
        let mut subs = Vec::new();
        for (_, g, ch, w) in self.defender_moves(st, &s, c) {
            let map = self.map(st, &ch);
            let bonus = u8::from(w);
            let vs = self.banded(b, ch, won + bonus, budget);
            if self.interrupted {
                return None;
            }
            let sub = maximal(
                vs.iter()
                    .map(|y| {
                        let mut p = Tricks::ZERO;
                        for i in g.iter() {
                            p.set(i, y.get(map[i]) + bonus);
                            clamp(&mut p, i);
                        }
                        p
                    })
                    .collect(),
                s.len,
                None,
            );
            for i in g.iter() {
                let most = sub.iter().map(|p| p.get(i)).max().unwrap_or(0);
                start.set(i, start.get(i).min(most));
            }
            let mut mask = Tricks::ZERO;
            for i in g.iter() {
                mask.set(i, 0xFF);
            }
            subs.push((mask, sub));
        }
        Some((start, subs))
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
    fn plan(&mut self, st: State, req: Tricks) -> Plan {
        let s = self.structure(st.live());
        if (0..s.len).all(|i| req.get(i) == 0) {
            return Plan::Made;
        }
        let c = Self::consistent(&st, &s);
        if Self::is_declarer_turn(&st) {
            for (ch, at, cards) in self.declarer_moves(&st, &s) {
                if self.guarantees(ch, &req, s.len) {
                    let then = self.plan(ch, req);
                    return Self::declarer_step(at, cards, then);
                }
            }
            assert!(self.interrupted, "req is feasible, so some move keeps it");
            return Plan::Hopeless;
        }
        let seat_index = (st.leader + st.played) % 4;
        let mut branches: Vec<(Vec<Option<Rank>>, Plan)> = Vec::new();
        let mut ruff = None;
        for (card, g, ch, won) in self.defender_moves(&st, &s, c) {
            let map = self.map(&st, &ch);
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
            Self::add_branch(&mut branches, &mut ruff, &s, card, sub);
        }
        Plan::Defender {
            seat: seat(seat_index),
            branches,
            ruff,
        }
    }

    /// Declarer's move `cards` (as [`declarer_moves`](Self::declarer_moves)
    /// gives it) from seat `at`, then `then`
    fn declarer_step(at: u8, cards: u16, then: Plan) -> Plan {
        let (seat, then) = (seat(at), Box::new(then));
        if cards == 1 << RUFF {
            Plan::Ruff { seat, then }
        } else {
            let cards = Holding::from_bits_truncate(cards);
            Plan::Declarer { seat, cards, then }
        }
    }

    /// Add a defender's `card` leading to `sub`: a ruff to `ruff`, else to
    /// `branches`, merged with any branch that goes on the same way
    fn add_branch(
        branches: &mut Vec<(Vec<Option<Rank>>, Plan)>,
        ruff: &mut Option<Box<Plan>>,
        s: &Structure,
        card: Card,
        sub: Plan,
    ) {
        let labels: Vec<Option<Rank>> = match card {
            Card::Class(k) => ranks(s.classes[k].ranks)
                .map(|r| Some(Rank::new(r)))
                .collect(),
            Card::Out => vec![None],
            Card::Ruff => {
                *ruff = Some(Box::new(sub));
                return;
            }
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
    target: u8,
    st: State,
    /// West's cards as dealt
    west: u16,
    /// Cards still held by East, West
    hands: [u16; 2],
    won: u8,
}

impl Game {
    /// Deal `west` the given cards (East gets the rest of the missing ones)
    /// and play for at least `target` tricks; not with [`Constraints::ruffs`]
    pub fn new(solver: &mut Solver, west: Holding, target: u8) -> Result<Self, Error> {
        solver.check(target)?;
        // ponytail: a Play cannot show a ruff yet; the analysis view
        // (phase 5) will need it
        if solver.constraints.ruffs != [0; 4] {
            return Err(Error::Unsupported);
        }
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
        Ok(Self {
            target,
            st,
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

    /// Declarer's options now, each scored by the most the target can still
    /// be made on after it; empty once the suit is played out
    pub fn moves(&self, solver: &mut Solver) -> Vec<Lead> {
        let s = solver.structure(self.st.live());
        solver
            .declarer_moves(&self.st, &s)
            .into_iter()
            .map(|(ch, at, cards)| Lead {
                seat: seat(at),
                cards: Holding::from_bits_truncate(cards),
                score: solver.value(ch, self.target, self.won).0,
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
    /// which declarer's target fares worst on the actual layout, breaking ties
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
        let mut best: Option<((bool, u64), usize)> = None;
        for (m, &(card, _, ch, trick)) in moves.iter().enumerate() {
            let legal = match card {
                Card::Class(k) => hand & s.classes[k].ranks != 0,
                Card::Out => hand == 0,
                Card::Ruff => false,
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
            let (vs, _) = solver.feasible_t(ch, self.target.saturating_sub(won), Cut::ALL);
            let outcome = vs.iter().any(|v| v.contains(layout));
            let key = (outcome, solver.value(ch, self.target, won).0);
            if best.is_none_or(|(k, _)| key < k) {
                best = Some((key, m));
            }
        }
        let (_, m) = best.expect("a defender can always follow or show out");
        let (card, _, ch, trick) = moves[m];
        let won = self.won + u8::from(trick);
        let card = match card {
            Card::Class(k) => Some(lowest(hand & s.classes[k].ranks)),
            _ => None,
        };
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
            Self::Ruff { seat, then } => {
                writeln!(f, "{pad}{seat:?} ruffs")?;
                then.fmt_indented(f, depth)
            }
            Self::Defender {
                seat,
                branches,
                ruff,
            } => {
                for (labels, plan) in branches {
                    let labels: Vec<String> = labels
                        .iter()
                        .map(|l| l.map_or("void".to_string(), |r| r.to_string()))
                        .collect();
                    writeln!(f, "{pad}{seat:?} {}:", labels.join("/"))?;
                    plan.fmt_indented(f, depth + 1)?;
                }
                if let Some(plan) = ruff {
                    writeln!(f, "{pad}{seat:?} ruffs:")?;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each card's value at a position equals the best of every way on
    /// from it, by the full search, at the first card and a trick later;
    /// the line's own card is worth at least nothing over it, and after a
    /// proven best line no card is worth more
    #[test]
    fn choices_match_every_line() {
        let imps = Scoring::Imps {
            contract: "3N".parse().unwrap(),
            vulnerable: true,
            need: 2,
        };
        let mut checked = 0;
        for (n, s) in [
            ("AQT98", "5432"),
            ("Q543", "AT2"),
            ("AJ98", "K32"),
            ("AJ52", "KT3"),
        ] {
            for scoring in [Scoring::Matchpoints, imps] {
                let mut solver = Solver::new(n.parse().unwrap(), s.parse().unwrap()).unwrap();
                let cmp = solver.compare(scoring).unwrap();
                let plan = &cmp.lines[0].plan;
                // The first trick by the plan, the first card of each choice
                let mut plays = Vec::new();
                let mut node = plan;
                while plays.len() < 4 {
                    match node {
                        Plan::Declarer { cards, then, .. } => {
                            plays.push(cards.iter().next());
                            node = then;
                        }
                        Plan::Defender { branches, .. } => {
                            plays.push(branches[0].0[0]);
                            node = &branches[0].1;
                        }
                        _ => break,
                    }
                }
                for k in [0, plays.len()] {
                    // A trick later the plan may be done
                    let Ok(got) = solver.choices(scoring, plan, &plays[..k]) else {
                        assert!(k > 0, "{n} / {s}: the first card has choices");
                        continue;
                    };
                    let (st, won, _, reach) = solver.replay(plan, &plays[..k]).unwrap();
                    let root = solver.root();
                    let rs = solver.structure(root.live());
                    let line = solver.follow(root, plan, None);
                    let off = solver.follow(root, plan, Some(&plays[..k]));
                    let str = solver.structure(st.live());
                    let c = Solver::consistent(&st, &str);
                    // Every way on from here, in place of the plan's
                    let net = |v: &Tricks| -> i64 {
                        (0..rs.len)
                            .filter_map(|r| reach[r].map(|j| (r, j)))
                            .filter(|&(_, j)| c.contains(j))
                            .map(|(r, j)| {
                                let x = (won + v.get(j)).min(off.get(r));
                                rs.weight[r] as i64 * scoring.net(x, line.get(r))
                            })
                            .sum()
                    };
                    let moves = solver.declarer_moves(&st, &str);
                    assert_eq!(moves.len(), got.choices.len());
                    for (&(ch, _, _), choice) in moves.iter().zip(&got.choices) {
                        let want = solver.feasible(ch, Cut::ALL).0.iter().map(net).max();
                        assert!(choice.exact);
                        assert_eq!(Some(choice.net), want, "{n} / {s} {scoring:?} after {k}");
                        // The plan's own way on is one of them.
                        if choice.line {
                            assert!(choice.net >= 0, "{n} / {s}: the line's own card");
                        }
                        if cmp.proven {
                            assert!(choice.net <= 0, "{n} / {s}: nothing beats the best");
                        }
                        checked += 1;
                    }
                    assert_eq!(got.choices.iter().filter(|c| c.line).count(), 1);
                }
            }
        }
        assert!(checked > 50, "{checked} choices checked");
    }

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
