//! Stopping a search early ([`Solver::set_interrupt`]): an interrupted
//! analysis bounds the optimum, and asking again on the same solver gives
//! exactly what a fresh solver gives.

use contract_bridge::Holding;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use suit_combination::{Analysis, Error, Goal, Solver};

fn holding(s: &str) -> Holding {
    if s == "-" {
        Holding::EMPTY
    } else {
        s.parse().unwrap()
    }
}

/// Seeded random combinations with `min..=max` cards missing
fn random_combos(count: usize, min: u64, max: u64, seed: u64) -> Vec<(Holding, Holding)> {
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
        let missing = (min + rand() % (max - min + 1)) as usize;
        let mut deck: Vec<u8> = (2..15).collect();
        for i in (1..deck.len()).rev() {
            deck.swap(i, (rand() % (i as u64 + 1)) as usize);
        }
        let declarer = &deck[..13 - missing];
        let (n, s) = declarer.split_at((rand() % (declarer.len() as u64 + 1)) as usize);
        let bits = |r: &[u8]| Holding::from_bits_truncate(r.iter().fold(0, |b, r| b | 1 << r));
        let combo = (bits(n), bits(s));
        if !out.contains(&combo) {
            out.push(combo);
        }
    }
    out
}

/// `f` of every item, spread over the machine's threads
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let out = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let r = f(item);
                    out.lock().unwrap().push((i, r));
                }
            });
        }
    });
    let mut out = out.into_inner().unwrap();
    out.sort_by_key(|&(i, _)| i);
    out.into_iter().map(|(_, r)| r).collect()
}

/// Every trick target, then expected tricks (and `exact`ly)
fn goals(north: Holding, south: Holding, exact: bool) -> Vec<Goal> {
    let max = north.len().max(south.len()) as u8;
    (1..=max)
        .map(|target| Goal::Tricks { target })
        .chain([Goal::Expected])
        .chain(exact.then_some(Goal::ExactExpected))
        .collect()
}

/// The next analysis on `solver` stops at the interrupt's `k`-th poll
fn stop_at(solver: &mut Solver, k: usize) {
    let mut polls = 0;
    solver.set_interrupt(move |_| {
        polls += 1;
        polls >= k
    });
}

/// An interrupted analysis may only bound the optimum from above: `best` is
/// the fresh analysis, `exact` the exact expected tricks if known
fn check_bound(
    r: Result<Analysis, Error>,
    best: &Analysis,
    exact: Option<u64>,
    what: &str,
) -> bool {
    match r {
        Ok(a) => {
            assert_eq!(&a, best, "{what}");
            false
        }
        Err(Error::Interrupted { bound }) => {
            assert!(
                bound >= best.bound,
                "{what}: bound {bound} < {}",
                best.bound
            );
            if !matches!(best.goal, Goal::Tricks { .. }) {
                assert!(
                    exact.is_none_or(|x| bound >= x),
                    "{what}: bound {bound} < exact"
                );
            }
            true
        }
        Err(e) => panic!("{what}: {e}"),
    }
}

/// Polls of a fresh search for `goal`, one every `every` memo misses
fn polls(north: Holding, south: Holding, goal: Goal, every: u32) -> usize {
    let count = std::rc::Rc::new(std::cell::Cell::new(0));
    let c = std::rc::Rc::clone(&count);
    let mut solver = Solver::new(north, south).unwrap();
    solver.set_poll_interval(every);
    solver.set_interrupt(move |_| {
        c.set(c.get() + 1);
        false
    });
    solver.analyze(goal).unwrap();
    count.get()
}

/// A combination's goals, each with its fresh analysis and polls
struct Fresh {
    north: Holding,
    south: Holding,
    goals: Vec<(Goal, Analysis, usize)>,
    /// Exact expected tricks, if among the goals
    exact: Option<u64>,
}

/// Steps of [`resume`]: the first three polls, then 13ths of the way
const STEPS: usize = 16;

/// Interrupt every goal of a combination at `step`, all on one solver
/// polled every `every` memo misses, then finish them there: each must
/// come out as from a fresh solver.  Step 0..3 stops at the first few
/// polls, the rest at 13ths of the way through each goal's fresh search,
/// down to its last poll.  Returns how many searches were interrupted.
fn resume(f: &Fresh, step: usize, every: u32) -> usize {
    let (north, south) = (f.north, f.south);
    let mut solver = Solver::new(north, south).unwrap();
    solver.set_poll_interval(every);
    let mut stopped = 0;
    for (goal, best, n) in &f.goals {
        let k = match step {
            0..3 => step + 1,
            _ => (n * (step - 2)).div_ceil(13).max(1),
        };
        stop_at(&mut solver, k);
        let what = format!("{north} {south} {goal:?} k={k}");
        stopped += usize::from(check_bound(solver.analyze(*goal), best, f.exact, &what));
    }
    solver.clear_interrupt();
    for (goal, best, _) in &f.goals {
        let a = solver.analyze(*goal).unwrap();
        assert_eq!(
            &a, best,
            "{north} {south} {goal:?} resumed after step {step}"
        );
    }
    stopped
}

/// [`resume`] every step of every combination
fn resume_all(combos: &[(Holding, Holding)], every: u32, exact: bool) -> usize {
    let fresh = par_map(combos, |&(north, south)| {
        let goals: Vec<_> = goals(north, south, exact)
            .into_iter()
            .map(|g| {
                let best = Solver::new(north, south).unwrap().analyze(g).unwrap();
                (g, best, polls(north, south, g, every))
            })
            .collect();
        let exact = exact.then(|| goals.last().unwrap().1.score);
        Fresh {
            north,
            south,
            goals,
            exact,
        }
    });
    let tasks: Vec<(&Fresh, usize)> = fresh
        .iter()
        .flat_map(|f| (0..STEPS).map(move |step| (f, step)))
        .collect();
    par_map(&tasks, |&(f, step)| resume(f, step, every))
        .iter()
        .sum()
}

#[test]
#[cfg_attr(debug_assertions, ignore = "slow unoptimized: run with --release")]
fn resumes_small_combinations_exactly() {
    let mut combos = random_combos(60, 3, 6, 0x5DEE_CE66_D1CE_4E5B);
    for (n, s) in [
        ("A8", "JT62"),
        ("AQ3", "42"),
        ("3", "AKT9"),
        ("AJ52", "K76"),
    ] {
        combos.push((holding(n), holding(s)));
    }
    let tried = resume_all(&combos, 1, true);
    assert!(tried > 3000, "only {tried} interruptions");
}

#[test]
#[cfg_attr(debug_assertions, ignore = "slow unoptimized: run with --release")]
fn resumes_hard_combinations_exactly() {
    // Polled every 1024 states, as by default
    let combos = [("A953", "KJ8"), ("KJ84", "T63")].map(|(n, s)| (holding(n), holding(s)));
    let tried = resume_all(&combos, 1024, false);
    assert!(tried > 100, "only {tried} interruptions");
}

#[test]
#[cfg_attr(debug_assertions, ignore = "slow unoptimized: run with --release")]
fn short_budgets_still_finish() {
    // Each call stops after 200 states (or 1024, the default, on the hard
    // one); the memo keeps every finished state, so calls make progress
    // and the last one is the fresh answer.
    let mut combos = random_combos(40, 4, 7, 0x2545_F491_4F6C_DD1D);
    combos.push((holding("A953"), holding("KJ8")));
    par_map(&combos, |&(n, s)| {
        let mut solver = Solver::new(n, s).unwrap();
        let k = if n.len() + s.len() == 7 {
            1
        } else {
            solver.set_poll_interval(1);
            200
        };
        for goal in goals(n, s, false) {
            let best = Solver::new(n, s).unwrap().analyze(goal).unwrap();
            let mut calls = 0;
            let a = loop {
                stop_at(&mut solver, k);
                calls += 1;
                match solver.analyze(goal) {
                    Ok(a) => break a,
                    Err(Error::Interrupted { bound }) => assert!(bound >= best.bound),
                    Err(e) => panic!("{e}"),
                }
                assert!(calls < 100_000, "{n} {s} {goal:?} makes no progress");
            };
            assert_eq!(a, best, "{n} {s} {goal:?}");
        }
    });
}

/// The bound of an analysis interrupted at once: double dummy, since no
/// target has finished
fn double_dummy(north: Holding, south: Holding, goal: Goal) -> Option<u64> {
    let mut solver = Solver::new(north, south).unwrap();
    solver.set_poll_interval(1);
    solver.set_interrupt(|_| true);
    match solver.analyze(goal) {
        Err(Error::Interrupted { bound }) => Some(bound),
        Ok(_) => None,
        Err(e) => panic!("{e}"),
    }
}

#[test]
#[cfg_attr(debug_assertions, ignore = "slow unoptimized: run with --release")]
fn double_dummy_bounds_single_dummy() {
    let combos = random_combos(300, 2, 8, 0x9E37_79B9_7F4A_7C15);
    let above: usize = par_map(&combos, |&(n, s)| {
        let mut above = 0;
        let mut solver = Solver::new(n, s).unwrap();
        for goal in goals(n, s, n.len() + s.len() >= 7) {
            let a = solver.analyze(goal).unwrap();
            if let Some(dd) = double_dummy(n, s, goal) {
                assert!(dd >= a.bound, "{n} {s} {goal:?}: {dd} < {}", a.bound);
                above += usize::from(dd > a.bound);
            }
        }
        above
    })
    .iter()
    .sum();
    assert!(above > 100, "double dummy is rarely above: {above}");
    // A two-way finesse for the queen: double dummy never guesses wrong.
    let (n, s) = (holding("AJ32"), holding("KT54"));
    let goal = Goal::Tricks { target: 4 };
    let sd = Solver::new(n, s).unwrap().analyze(goal).unwrap().score;
    assert!(double_dummy(n, s, goal).unwrap() > sd);
    // No guess: AKQ32 opposite 54 makes five tricks on 3-3 either way.
    let (n, s) = (holding("AKQ32"), holding("54"));
    let goal = Goal::Tricks { target: 5 };
    let sd = Solver::new(n, s).unwrap().analyze(goal).unwrap().score;
    assert_eq!(double_dummy(n, s, goal), Some(sd));
}

#[test]
#[cfg_attr(debug_assertions, ignore = "slow unoptimized: run with --release")]
fn lead_bounds_hold() {
    let mut combos = random_combos(150, 3, 6, 0x2545_F491_4F6C_DD1D);
    combos.extend([("A8", "JT62"), ("3", "AKT9")].map(|(n, s)| (holding(n), holding(s))));
    par_map(&combos, |&(n, s)| {
        let mut solver = Solver::new(n, s).unwrap();
        for goal in goals(n, s, false) {
            let a = solver.analyze(goal).unwrap();
            assert_eq!(a.proven, a.score == a.bound);
            for l in &a.leads {
                assert!(
                    l.score <= l.bound && l.bound <= a.bound,
                    "{n} {s} {goal:?} {l:?}"
                );
                if goal != Goal::Expected {
                    assert_eq!(l.score, l.bound);
                }
            }
        }
        // Each lead's bound holds the best line after that lead.
        let line = solver.analyze(Goal::Expected).unwrap();
        let exact = solver.analyze(Goal::ExactExpected).unwrap();
        assert!(exact.proven && exact.leads.iter().all(|l| l.score == l.bound));
        for l in &line.leads {
            let best = exact
                .leads
                .iter()
                .find(|x| x.seat == l.seat && x.cards == l.cards)
                .unwrap();
            assert!(
                l.score <= best.score && best.score <= l.bound,
                "{n} {s} {l:?} {best:?}"
            );
        }
    });
}
