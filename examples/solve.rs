//! `cargo run --release --example solve -- AJ52 K76 [TARGET|ev|exact] [--budget SECS]`
//!
//! Prints the value of every goal (or just the one given) with its status,
//! and the line for the goal shown last.  `--budget` gives each goal at most
//! that many seconds; a goal out of time shows only its bound.

use contract_bridge::Holding;
use std::time::{Duration, Instant};
use suit_combination::{Error, Goal, Solver, TOTAL};

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

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let budget = args.iter().position(|a| a == "--budget").map(|i| {
        let secs = args.get(i + 1).and_then(|s| s.parse::<f64>().ok());
        let secs = secs.unwrap_or_else(|| fail("--budget: seconds expected"));
        args.drain(i..i + 2);
        Duration::from_secs_f64(secs)
    });
    let [north, south, rest @ ..] = args.as_slice() else {
        fail("usage: solve NORTH SOUTH [TARGET|ev|exact] [--budget SECS]");
    };
    let (north, south) = (holding("north", north), holding("south", south));
    let mut solver = Solver::new(north, south).unwrap_or_else(|e| fail(e));
    let max = north.len().max(south.len()) as u8;
    let goals = match rest {
        [t] if t == "ev" => vec![Goal::Expected],
        [t] if t == "exact" => vec![Goal::ExactExpected],
        [t] => vec![Goal::Tricks {
            target: t.parse().unwrap_or_else(|_| fail(format!("target: {t}?"))),
        }],
        [] => (1..=max)
            .map(|target| Goal::Tricks { target })
            .chain([Goal::Expected])
            .collect(),
        _ => fail("usage: solve NORTH SOUTH [TARGET|ev|exact] [--budget SECS]"),
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
                Err(e @ Error::BadTarget) => fail(e),
                r => (goal, r),
            }
        })
        .collect();
    let elapsed = start.elapsed();
    let value = |goal: Goal, score: u64| match goal {
        Goal::Tricks { .. } => format!("{:.2}%", score as f64 / TOTAL as f64 * 100.0),
        _ => format!("{:.3} tricks", score as f64 / TOTAL as f64),
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
            Err(Error::Interrupted { bound }) => {
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
        .find(|a| !matches!(a.goal, Goal::Tricks { .. }) || (a.score < TOTAL && a.score > 0))
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
