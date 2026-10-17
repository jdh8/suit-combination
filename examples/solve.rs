//! `cargo run --release --example solve -- AJ52 K76 [TARGET|ev|exact] [--budget SECS]
//! [--vacant EAST,WEST] [--east CARDS] [--west CARDS] [--length EAST,WEST]
//! [--hcp EAST,WEST] [--first N|S] [--entries NORTH,SOUTH]`
//!
//! Prints the value of every goal (or just the one given) with its status,
//! and the line for the goal shown last.  `--budget` gives each goal at most
//! that many seconds; a goal out of time shows only its bound.  `--vacant`
//! sets the defenders' vacant places, `--east` and `--west` the cards known
//! to be with them, `--length` and `--hcp` (this suit only) ranges such as
//! `3`, `2-4`, `-2`, `3-` or `-` for any, e.g. `--length -,-1`.  `--first`
//! names the hand the lead starts in, `--entries` each hand's entries in
//! other suits, `-` for unlimited, e.g. `--entries -,1`.  Only trick targets
//! take these.

use contract_bridge::{Holding, Seat};
use std::time::{Duration, Instant};
use suit_combination::{Constraints, Error, Goal, Solver};

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}

fn holding(seat: &str, s: &str) -> Holding {
    if s == "-" {
        return Holding::EMPTY;
    }
    s.parse().unwrap_or_else(|e| {
        fail(format!(
            "{seat}: {e} (ranks in descending order, e.g. AJ52)"
        ))
    })
}

const USAGE: &str = "usage: solve NORTH SOUTH [TARGET|ev|exact] [--budget SECS] \
    [--vacant EAST,WEST] [--east CARDS] [--west CARDS] [--length EAST,WEST] [--hcp EAST,WEST] \
    [--first N|S] [--entries NORTH,SOUTH]";

/// Removes `--flag VALUE` from `args`, returning the value
fn flag(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    if i + 1 >= args.len() {
        fail(format!("{name}: value expected"));
    }
    Some(args.drain(i..i + 2).nth(1).unwrap())
}

/// `EAST,WEST` ranges for `name`, each `LO-HI` with either end left out
/// for `default`'s, or one number
fn ranges(name: &str, value: &str, default: [u8; 2]) -> [[u8; 2]; 2] {
    let side = |s: &str| -> Option<[u8; 2]> {
        let end = |e: &str, d| {
            if e.is_empty() {
                Some(d)
            } else {
                e.parse().ok()
            }
        };
        match s.split_once('-') {
            Some((lo, hi)) => Some([end(lo, default[0])?, end(hi, default[1])?]),
            None if s.is_empty() => Some(default),
            None => s.parse().ok().map(|n| [n, n]),
        }
    };
    let sides: Option<Vec<_>> = value.split(',').map(side).collect();
    match sides.as_deref() {
        Some(&[east, west]) => [east, west],
        _ => fail(format!("{name}: EAST,WEST ranges expected, e.g. -,2-4")),
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let budget = flag(&mut args, "--budget").map(|s| {
        let secs = s.parse::<f64>().ok();
        Duration::from_secs_f64(secs.unwrap_or_else(|| fail("--budget: seconds expected")))
    });
    let mut constraints = Constraints::default();
    if let Some(v) = flag(&mut args, "--vacant") {
        let places: Vec<_> = v.split(',').filter_map(|p| p.parse::<u8>().ok()).collect();
        let [east, west] = places[..] else {
            fail("--vacant: EAST,WEST expected, e.g. 9,6");
        };
        constraints.vacant = [east, west];
    }
    for (i, name) in ["--east", "--west"].into_iter().enumerate() {
        if let Some(cards) = flag(&mut args, name) {
            constraints.known[i] = holding(&name[2..], &cards);
        }
    }
    let d = Constraints::default();
    if let Some(v) = flag(&mut args, "--length") {
        constraints.length = ranges("--length", &v, d.length[0]);
    }
    if let Some(v) = flag(&mut args, "--hcp") {
        constraints.hcp = ranges("--hcp", &v, d.hcp[0]);
    }
    if let Some(v) = flag(&mut args, "--first") {
        constraints.first = Some(match v.as_str() {
            "N" => Seat::North,
            "S" => Seat::South,
            _ => fail("--first: N or S expected"),
        });
    }
    if let Some(v) = flag(&mut args, "--entries") {
        let hand = |e: &str| {
            (e == "-")
                .then_some(None)
                .or_else(|| e.parse().ok().map(Some))
        };
        let entries: Option<Vec<_>> = v.split(',').map(hand).collect();
        let Some(&[north, south]) = entries.as_deref() else {
            fail("--entries: NORTH,SOUTH expected, e.g. -,1");
        };
        constraints.entries = [north, south];
    }
    let [north, south, rest @ ..] = args.as_slice() else {
        fail(USAGE);
    };
    let (north, south) = (holding("north", north), holding("south", south));
    let mut solver =
        Solver::with_constraints(north, south, constraints).unwrap_or_else(|e| fail(e));
    let total = solver.total();
    let max = north.len().max(south.len()) as u8;
    let goals = match rest {
        [t] if t == "ev" => vec![Goal::Expected],
        [t] if t == "exact" => vec![Goal::ExactExpected],
        [t] => vec![Goal::Tricks {
            target: t.parse().unwrap_or_else(|_| fail(format!("target: {t}?"))),
        }],
        [] => (1..=max)
            .map(|target| Goal::Tricks { target })
            .chain((constraints == Constraints::default()).then_some(Goal::Expected))
            .collect(),
        _ => fail(USAGE),
    };
    let start = Instant::now();
    let results: Vec<_> = goals
        .iter()
        .map(|&goal| {
            if let Some(budget) = budget {
                let deadline = Instant::now() + budget;
                solver.set_interrupt(move |_| Instant::now() >= deadline);
            }
            match solver.analyze(goal) {
                Err(e @ (Error::BadTarget | Error::Unsupported)) => fail(e),
                r => (goal, r),
            }
        })
        .collect();
    let elapsed = start.elapsed();
    let value = |goal: Goal, score: u64| match goal {
        Goal::Tricks { .. } => format!("{:.2}%", score as f64 / total as f64 * 100.0),
        _ => format!("{:.3} tricks", score as f64 / total as f64),
    };
    let name = |goal: Goal| match goal {
        Goal::Tricks { target } => format!("{target} tricks"),
        _ => "expected".to_string(),
    };
    println!("{north} opposite {south}");
    for (goal, r) in &results {
        match r {
            Ok(a) if a.proven => println!("{}: {} proven", name(*goal), value(*goal, a.score)),
            Ok(a) => println!(
                "{}: {}, best <= {}",
                name(*goal),
                value(*goal, a.score),
                value(*goal, a.bound)
            ),
            Err(Error::Interrupted { bound, .. }) => {
                println!(
                    "{}: interrupted (best <= {})",
                    name(*goal),
                    value(*goal, *bound)
                );
            }
            Err(e) => fail(e),
        }
    }
    // Show the line for the last nontrivial goal (every trick or no trick is no decision).
    if let Some(a) = results
        .iter()
        .rev()
        .filter_map(|(_, r)| r.as_ref().ok())
        .find(|a| !matches!(a.goal, Goal::Tricks { .. }) || (a.score < total && a.score > 0))
    {
        println!("\nLine for {}: {}", name(a.goal), value(a.goal, a.score));
        for l in &a.leads {
            let bound = if l.bound > l.score {
                format!(" (best after it <= {})", value(a.goal, l.bound))
            } else {
                String::new()
            };
            println!(
                "  lead {} from {:?}: {}{bound}",
                l.cards,
                l.seat,
                value(a.goal, l.score)
            );
        }
        print!("{}", a.plan);
    }
    eprintln!("({elapsed:?})");
}
