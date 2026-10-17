//! Reference cases from `tests/fixtures/cases.json`, and a policy evaluator
//! that replays a returned [`Plan`] on every layout, letting the defenders
//! try every legal card, without the engine's layout compression.

use contract_bridge::{Contract, Holding, Rank, Seat};
use serde::Deserialize;
use suit_combination::{Analysis, Constraints, Goal, Layout, Plan, Scoring, Solver, TOTAL};

#[derive(Deserialize)]
#[serde(untagged)]
enum CaseGoal {
    Tricks(u8),
    Expected(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    north: String,
    south: String,
    goal: CaseGoal,
    /// Vacant places of East, West (default 13, 13)
    vacant: Option<[u8; 2]>,
    /// Cards known to be with East, West
    east: Option<String>,
    west: Option<String>,
    /// Fewest and most cards of East, West
    length: Option<[[u8; 2]; 2]>,
    /// Fewest and most HCP of East, West in this suit
    hcp: Option<[[u8; 2]; 2]>,
    /// Hand that must lead first, "N" or "S"
    first: Option<String>,
    /// Outside entries to North, South; null = unlimited
    entries: Option<[Option<u8>; 2]>,
    /// Rounds played before the analysis stops
    rounds: Option<u8>,
    /// Card declarer must lead first
    lead: Option<String>,
    /// Declarer's cards kept out of play
    discard: Option<String>,
    /// Rounds North, East, South, West may ruff
    ruffs: Option<[u8; 4]>,
    score: Option<u64>,
    /// The constrained denominator; [`TOTAL`] if absent
    total: Option<u64>,
    value: Option<f64>,
    tolerance: Option<f64>,
    leads: Option<Vec<String>>,
    avoid: Option<Vec<String>>,
    #[allow(dead_code)]
    source: String,
    #[allow(dead_code)]
    checked: String,
    #[allow(dead_code)]
    note: Option<String>,
}

fn cases() -> Vec<Case> {
    serde_json::from_str(include_str!("fixtures/cases.json")).unwrap()
}

impl Case {
    fn constraints(&self) -> Constraints {
        let known = |h: &Option<String>| h.as_deref().map_or(Holding::EMPTY, holding);
        let d = Constraints::default();
        Constraints {
            vacant: self.vacant.unwrap_or(d.vacant),
            known: [known(&self.east), known(&self.west)],
            length: self.length.unwrap_or(d.length),
            hcp: self.hcp.unwrap_or(d.hcp),
            first: self.first.as_deref().map(|s| match s {
                "N" => Seat::North,
                "S" => Seat::South,
                _ => panic!("first: {s}?"),
            }),
            entries: self.entries.unwrap_or(d.entries),
            rounds: self.rounds.unwrap_or(d.rounds),
            lead: self.lead.as_deref().map(|r| r.parse().unwrap()),
            discards: known(&self.discard),
            ruffs: self.ruffs.unwrap_or(d.ruffs),
        }
    }
}

fn binom(n: u64, k: u64) -> u64 {
    (0..k).fold(1, |acc, i| acc * (n - i) / (i + 1))
}

fn seat_index(seat: Seat) -> usize {
    match seat {
        Seat::North => 0,
        Seat::East => 1,
        Seat::South => 2,
        Seat::West => 3,
    }
}

/// The suit mid-play: hands by seat index, the trick so far, tricks won
#[derive(Clone, Copy)]
struct Pos {
    hands: [u16; 4],
    leader: usize,
    trick: [u8; 4],
    played: usize,
    won: u8,
    /// Hands that may lead the next trick without an entry: the one that
    /// won the last trick, or the one the lead starts in (either if none)
    free: [bool; 2],
    /// Outside entries left to North, South; `None` = unlimited
    entries: [Option<u8>; 2],
    /// Rounds left to play
    rounds: u8,
    /// Card the first lead must be (or be equivalent to)
    fixed: Option<u8>,
    /// Ruffs left by seat index
    ruffs: [u8; 4],
}

/// A ruff in [`Pos::trick`]: above every card of the suit
const RUFF: u8 = 15;

impl Pos {
    /// Declarer's hand `h` (0 North, 1 South) leads one of `cards`,
    /// spending an entry unless it may lead free
    fn lead(&mut self, h: usize, cards: u16) {
        assert!(self.rounds > 0, "a round past the last");
        if let Some(r) = self.fixed.take() {
            assert!(cards >> r & 1 != 0, "not the fixed lead");
        }
        if !self.free[h]
            && let Some(e) = &mut self.entries[h]
        {
            *e = e.checked_sub(1).expect("no entry left");
        }
    }

    /// Seat `at` plays `rank` (0 = discards or shows out)
    fn play(mut self, at: usize, rank: u8) -> Self {
        assert_eq!((self.leader + self.played) % 4, at, "out of turn");
        self.hands[at] &= !(1 << rank);
        self.trick[self.played] = rank;
        self.played += 1;
        if self.played == 4 {
            let winner = (0..4).max_by_key(|&p| self.trick[p]).unwrap();
            let seat = (self.leader + winner) % 4;
            self.won += u8::from(seat.is_multiple_of(2));
            self.free = [seat == 0, seat == 2];
            self.rounds -= 1;
            self.trick = [0; 4];
            self.played = 0;
        }
        self
    }
}

/// Fewest tricks declarer has won where `plan` ends, when the defenders
/// answer every play with every card they legally can
fn follow(plan: &Plan, mut p: Pos) -> u8 {
    match plan {
        Plan::Made | Plan::Hopeless => p.won,
        Plan::Declarer { seat, cards, then } => {
            let at = seat_index(*seat);
            if p.played == 0 {
                p.leader = at;
                p.lead(at / 2, cards.to_bits());
            }
            let choice = cards.to_bits() & p.hands[at];
            let rank = if choice == 0 {
                assert!(cards.is_empty() && p.hands[at] == 0, "card not held");
                0 // discard
            } else {
                choice.trailing_zeros() as u8
            };
            follow(then, p.play(at, rank))
        }
        Plan::Ruff { seat, then } => {
            let at = seat_index(*seat);
            assert!(p.played > 0 && p.hands[at] == 0, "ruff while not void");
            p.ruffs[at] = p.ruffs[at].checked_sub(1).expect("no ruff left");
            follow(then, p.play(at, RUFF))
        }
        Plan::Defender {
            seat,
            branches,
            ruff,
        } => {
            let at = seat_index(*seat);
            let hand = p.hands[at];
            let options: Vec<Option<u8>> = if hand == 0 {
                vec![None]
            } else {
                (2..15).filter(|r| hand >> r & 1 != 0).map(Some).collect()
            };
            let ruffed = (hand == 0 && p.ruffs[at] > 0).then(|| {
                let mut p = p;
                p.ruffs[at] -= 1;
                follow(ruff.as_ref().expect("plan covers the ruff"), p.play(at, RUFF))
            });
            options
                .into_iter()
                .map(|o| {
                    let label = o.map(Rank::new);
                    let (_, sub) = branches
                        .iter()
                        .find(|(labels, _)| labels.contains(&label))
                        .expect("plan covers every defence");
                    follow(sub, p.play(at, o.unwrap_or(0)))
                })
                .chain(ruffed)
                .min()
                .unwrap()
        }
    }
}

/// Deals in which West holds exactly `west` of the `missing` cards under
/// `c`: none unless the known cards are where `c` puts them and the
/// lengths and HCP are in range, else the ways West's other vacant places
/// take the defenders' unknown cards of other suits
fn weight(missing: u16, west: u16, c: &Constraints) -> u64 {
    let [east_known, west_known] = c.known.map(Holding::to_bits);
    if west & west_known != west_known || west & east_known != 0 {
        return 0;
    }
    let n = u64::from(missing.count_ones());
    let w = u64::from(west.count_ones());
    let hcp = |m: u16| (11..15).filter(|&r| m >> r & 1 != 0).map(|r| r - 10).sum::<u64>();
    let held = [[n - w, w], [hcp(missing & !west), hcp(west)]];
    let within = |[lo, hi]: [u8; 2], x: u64| (u64::from(lo)..=u64::from(hi)).contains(&x);
    if (0..2).any(|d| !within(c.length[d], held[0][d]) || !within(c.hcp[d], held[1][d])) {
        return 0;
    }
    let [east, west] = c.vacant.map(u64::from);
    if w > west || n - w > east {
        return 0;
    }
    binom(east + west - n, west - w)
}

/// The plan's score recomputed over every layout: deals reaching the target,
/// or tricks times deals.  Each deal's tricks must agree with the
/// guarantee of its layout of `layouts`.
fn evaluate(a: &Analysis, c: &Constraints, layouts: &[Layout]) -> u64 {
    let missing = (Holding::ALL - a.north - a.south).to_bits();
    let mut west = 0u16;
    let mut total = 0;
    loop {
        let p = Pos {
            hands: [
                (a.north - c.discards).to_bits(),
                missing & !west,
                (a.south - c.discards).to_bits(),
                west,
            ],
            leader: 0,
            trick: [0; 4],
            played: 0,
            won: 0,
            free: [Seat::North, Seat::South].map(|h| c.first.is_none_or(|f| f == h)),
            entries: c.entries,
            rounds: c.rounds,
            fixed: c.lead.map(Rank::get),
            ruffs: c.ruffs,
        };
        let weight = weight(missing, west, c);
        let tricks = if weight == 0 { 0 } else { follow(&a.plan, p) };
        if weight > 0 {
            let i = layouts
                .iter()
                .position(|l| {
                    l.west
                        .iter()
                        .all(|&(h, n)| (h.to_bits() & west).count_ones() == u32::from(n))
                })
                .expect("every deal has a layout");
            let g = a.guarantee[i];
            let what = format!("{} / {} {:?} West {west:#x}", a.north, a.south, a.goal);
            match a.goal {
                Goal::Tricks { target } => assert_eq!(tricks >= target, g >= target, "{what}"),
                Goal::ExactExpected => assert_eq!(tricks, g, "{what}"),
            }
        }
        total += weight
            * match a.goal {
                Goal::Tricks { target } => u64::from(tricks >= target),
                _ => u64::from(tricks),
            };
        if west == missing {
            return total;
        }
        west = (west.wrapping_sub(missing)) & missing;
    }
}

/// A holding as text, `-` for a void
fn holding(s: &str) -> Holding {
    if s == "-" {
        Holding::EMPTY
    } else {
        s.parse().unwrap()
    }
}

fn lead_score(a: &Analysis, rank: &str) -> u64 {
    let rank: Rank = rank.parse().unwrap();
    a.leads
        .iter()
        .find(|l| l.cards.contains(rank))
        .unwrap_or_else(|| panic!("{rank} is not a lead of {} / {}", a.north, a.south))
        .score
}

#[test]
fn reference_cases() {
    let mut failures = Vec::new();
    for c in cases() {
        let (n, s) = (holding(&c.north), holding(&c.south));
        let goal = match c.goal {
            CaseGoal::Tricks(t) => Goal::Tricks { target: t },
            CaseGoal::Expected(ref tag) => {
                assert_eq!(tag, "expected", "{} / {}", c.north, c.south);
                Goal::ExactExpected
            }
        };
        let constraints = c.constraints();
        let mut solver = Solver::with_constraints(n, s, constraints).unwrap();
        let a = solver.analyze(goal).unwrap();
        let name = format!("{n} / {s} {goal:?} {constraints:?}");
        let mut fail = |what: String| failures.push(format!("{name}: {what}"));
        let total = c.total.unwrap_or(TOTAL);
        if a.total != total {
            fail(format!("total {} != {total}", a.total));
        }
        if let Some(score) = c.score
            && a.score != score
        {
            fail(format!("score {} != {score}", a.score));
        }
        if let Some(value) = c.value {
            let tolerance = c.tolerance.unwrap_or(0.00005);
            if (a.value() - value).abs() > tolerance {
                fail(format!("value {:.5} not within {tolerance} of {value}", a.value()));
            }
        }
        for lead in c.leads.iter().flatten() {
            if lead_score(&a, lead) != a.score {
                fail(format!("lead {lead} scores {} < {}", lead_score(&a, lead), a.score));
            }
        }
        for lead in c.avoid.iter().flatten() {
            if lead_score(&a, lead) >= a.score {
                fail(format!("lead {lead} is optimal but should be avoided"));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// Every goal's plan, replayed against every defence, scores what the
/// analysis claims
fn check_plans(north: Holding, south: Holding, c: Constraints) {
    let mut solver = Solver::with_constraints(north, south, c).unwrap();
    for a in &solver.analyze_all().unwrap() {
        let name = format!("{north} / {south} {:?} {c:?}", a.goal);
        assert_eq!(evaluate(a, &c, &solver.layouts()), a.score, "{name}");
    }
}

#[test]
fn reference_plans_hold_against_every_defence() {
    for c in cases() {
        check_plans(holding(&c.north), holding(&c.south), c.constraints());
    }
}

#[test]
fn random_plans_hold_against_every_defence() {
    // ponytail: xorshift64, so the sweep is the same every run
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let mut rand = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut done = 0;
    while done < 60 {
        let missing = 4 + (rand() % 4) as u32; // 4..=7 missing cards
        let mut deck: Vec<u8> = (2..15).collect();
        for i in (1..deck.len()).rev() {
            deck.swap(i, (rand() % (i as u64 + 1)) as usize);
        }
        let declarer = &deck[..13 - missing as usize];
        let north_len = (rand() % (declarer.len() as u64 + 1)) as usize;
        let (n, s) = declarer.split_at(north_len);
        let bits = |ranks: &[u8]| Holding::from_bits_truncate(ranks.iter().fold(0, |b, r| b | 1 << r));
        let (n, s) = (bits(n), bits(s));
        if n.len() < s.len() || s.is_empty() {
            continue;
        }
        check_plans(n, s, Constraints::default());
        done += 1;
    }
}

#[test]
fn total_is_the_sum_of_layout_weights() {
    let n = 8;
    let sum: u64 = (0..=n).map(|w| binom(n, w) * binom(26 - n, 13 - w)).sum();
    assert_eq!(sum, TOTAL);
}

/// How a [`Comparison`](suit_combination::Comparison) case scores: `"matchpoints"`, or IMPs
#[derive(Deserialize)]
#[serde(untagged)]
enum CaseScoring {
    Matchpoints(String),
    Imps {
        contract: String,
        vulnerable: bool,
        need: u8,
    },
}

/// The line a comparison case measures the best one against: the most
/// expected tricks, or of the lines best for a target, the most expected
/// tricks
#[derive(Deserialize)]
#[serde(untagged)]
enum Versus {
    Target(u8),
    Expected(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Comparison {
    north: String,
    south: String,
    scoring: CaseScoring,
    versus: Option<Versus>,
    /// What the best line wins over the `versus` line, and loses to it
    gain: Option<[u64; 2]>,
    /// One best line
    single: bool,
    #[allow(dead_code)]
    source: String,
    #[allow(dead_code)]
    checked: String,
    #[allow(dead_code)]
    note: Option<String>,
}

#[test]
fn comparison_cases() {
    let cases: Vec<Comparison> =
        serde_json::from_str(include_str!("fixtures/comparisons.json")).unwrap();
    for case in cases {
        let (n, s) = (holding(&case.north), holding(&case.south));
        let scoring = match case.scoring {
            CaseScoring::Matchpoints(ref m) => {
                assert_eq!(m, "matchpoints");
                Scoring::Matchpoints
            }
            CaseScoring::Imps {
                ref contract,
                vulnerable,
                need,
            } => Scoring::Imps {
                contract: contract.parse::<Contract>().unwrap(),
                vulnerable,
                need,
            },
        };
        let name = format!("{n} / {s} {scoring:?}");
        let mut solver = Solver::new(n, s).unwrap();
        let c = solver.compare(scoring).unwrap();
        let layouts = solver.layouts();
        // Each line plays as its trick distribution says
        for l in &c.lines {
            let a = |goal| Analysis {
                north: n,
                south: s,
                goal,
                score: l.score,
                leads: Vec::new(),
                plan: l.plan.clone(),
                guarantee: l.guarantee.clone(),
                total: c.total,
            };
            let d = Constraints::default();
            assert_eq!(evaluate(&a(Goal::ExactExpected), &d, &layouts), l.score, "{name}");
            for k in 1..l.tricks.len() {
                let want: u64 = l.tricks[k..].iter().sum();
                let target = Goal::Tricks { target: k as u8 };
                assert_eq!(evaluate(&a(target), &d, &layouts), want, "{name} {k} tricks");
            }
        }
        assert_eq!(c.best.len() == 1, case.single, "{name}: best {:?}", c.best);
        if let (Some(versus), Some(gain)) = (case.versus, case.gain) {
            // Lines come most expected tricks first
            let v = match versus {
                Versus::Expected(ref e) => {
                    assert_eq!(e, "expected");
                    0
                }
                Versus::Target(k) => {
                    let p = |i: usize| c.lines[i].tricks[usize::from(k)..].iter().sum::<u64>();
                    let most = (0..c.lines.len()).map(p).max().unwrap();
                    (0..c.lines.len()).find(|&i| p(i) == most).unwrap()
                }
            };
            let b = c.best[0];
            assert_eq!([c.gain[b][v], c.gain[v][b]], gain, "{name}");
        }
    }
}
