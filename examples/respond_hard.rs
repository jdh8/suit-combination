//! Prototype: banded best response against each target's line.  usage: respond_hard NORTH SOUTH
use std::time::Instant;
use suit_combination::{Scoring, Solver};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let parse = |s: &str| {
        if s == "-" {
            "".parse().unwrap()
        } else {
            s.parse().unwrap()
        }
    };
    let (n, s) = (parse(&a[0]), parse(&a[1]));
    let mut solver = Solver::new(n, s).unwrap();
    for k in 1..=solver.max_tricks() {
        let mut g = solver.analyze(k).unwrap().guarantee;
        // Best response to the target line, then to that, a few times
        for round in 0..4 {
            let mut fresh = Solver::new(n, s).unwrap();
            let t = Instant::now();
            let (got, front, z) = fresh.respond(Scoring::Matchpoints, &g, 1);
            println!(
                "target {k} round {round}: beaten by {got:?} ({front} classes) in {:?}",
                t.elapsed()
            );
            if got.is_none() {
                break;
            }
            g = z;
        }
    }
}
