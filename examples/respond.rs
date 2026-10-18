//! Prototype check: best response by the banded search against the exact
//! comparison.  usage: respond NORTH SOUTH
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
    let scoring = Scoring::Matchpoints;
    let mut solver = Solver::new(n, s).unwrap();
    let t = Instant::now();
    let c = solver.compare(scoring).unwrap();
    println!("compare: {} lines in {:?}", c.lines.len(), t.elapsed());
    for &k in &[c.best[0], 0, c.lines.len() / 2, c.lines.len() - 1] {
        let want = (0..c.lines.len())
            .map(|x| c.gain(x, k) as i64 - c.gain(k, x) as i64)
            .max()
            .unwrap();
        let mut fresh = Solver::new(n, s).unwrap();
        let t = Instant::now();
        let (got, front, _) = fresh.respond(scoring, &c.lines[k].guarantee, 0);
        let ties = (0..c.lines.len())
            .filter(|&x| c.gain(x, k) >= c.gain(k, x))
            .count();
        println!(
            "line {k}: exact {want} banded {got:?} ({front} classes, {ties} lines tie or beat) in {:?}",
            t.elapsed()
        );
        assert_eq!(Some(want), got);
        let mut fresh = Solver::new(n, s).unwrap();
        let t = Instant::now();
        let (got, front, _) = fresh.respond(scoring, &c.lines[k].guarantee, 1);
        println!(
            "   beating it: {got:?} ({front} classes) in {:?}",
            t.elapsed()
        );
        assert_eq!(want > 0, got.is_some());
    }
}
