//! `cargo run --release --example solve -- AJ52 K76 [TARGET|ev|exact]`
//!
//! Prints the value of every goal (or just the one given) and the optimal
//! line for the goal shown last.

use contract_bridge::Holding;
use suit_combination::{Goal, TOTAL, analyze, analyze_all};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [north, south, rest @ ..] = args.as_slice() else {
        eprintln!("usage: solve NORTH SOUTH [TARGET|ev|exact]");
        std::process::exit(2);
    };
    let north: Holding = north
        .parse()
        .expect("north: ranks in descending order, e.g. AJ52");
    let south: Holding = south
        .parse()
        .expect("south: ranks in descending order, e.g. K76");
    let start = std::time::Instant::now();
    let analyses = match rest {
        [t] if t == "ev" => vec![analyze(north, south, Goal::Expected).unwrap()],
        [t] if t == "exact" => vec![analyze(north, south, Goal::ExactExpected).unwrap()],
        [t] => vec![
            analyze(
                north,
                south,
                Goal::Tricks {
                    target: t.parse().expect("target"),
                },
            )
            .unwrap(),
        ],
        _ => analyze_all(north, south).unwrap(),
    };
    let elapsed = start.elapsed();
    let show = |goal: Goal, score: u64| match goal {
        Goal::Tricks { target } => format!(
            "{target} tricks: {:.2}%",
            score as f64 / TOTAL as f64 * 100.0
        ),
        _ => format!("expected: {:.3} tricks", score as f64 / TOTAL as f64),
    };
    println!("{north} opposite {south}");
    for a in &analyses {
        if a.bound > a.score {
            let best = a.bound as f64 / TOTAL as f64;
            println!("{} (the best is at most {best:.3})", show(a.goal, a.score));
        } else {
            println!("{}", show(a.goal, a.score));
        }
    }
    // Show the line for the last nontrivial goal (every trick or no trick is no decision).
    if let Some(a) = analyses
        .iter()
        .rev()
        .find(|a| !matches!(a.goal, Goal::Tricks { .. }) || (a.score < TOTAL && a.score > 0))
    {
        println!("\nLine for {}", show(a.goal, a.score));
        for l in &a.leads {
            println!(
                "  lead {} from {:?}: {}",
                l.cards,
                l.seat,
                show(a.goal, l.score)
            );
        }
        print!("{}", a.plan);
    }
    eprintln!("({elapsed:?})");
}
