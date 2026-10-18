//! Prototype: the best of a beam's lines, then whether any line beats it.
//! usage: respond_beam NORTH SOUTH [WIDTH]
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
    let width = a.get(2).map_or(64, |w| w.parse().unwrap());
    let scoring = Scoring::Matchpoints;
    let mut solver = Solver::new(n, s).unwrap();
    let w: Vec<u64> = solver.layouts().iter().map(|l| l.weight).collect();
    let t = Instant::now();
    let lines = solver.beam_lines(width);
    println!("beam: {} lines in {:?}", lines.len(), t.elapsed());
    let d = |x: &[u8], y: &[u8]| -> i64 {
        (0..w.len())
            .map(|i| w[i] as i64 * (i64::from(x[i] > y[i]) - i64::from(x[i] < y[i])))
            .sum()
    };
    // Copeland winner of the beam
    let mut c = (0..lines.len())
        .max_by_key(|&a| {
            (0..lines.len())
                .filter(|&b| d(&lines[a], &lines[b]) >= 0)
                .count()
        })
        .map(|a| lines[a].clone())
        .unwrap();
    let total: i64 = w.iter().sum::<u64>() as i64;
    'rounds: for round in 0..6 {
        // Aspiration: look for a line beating `c` by much first
        for div in [8, 64, 512, 4096, total] {
            let least = (total / div).max(1);
            let mut fresh = Solver::new(n, s).unwrap();
            let t = Instant::now();
            let (got, front, z) = fresh.respond(scoring, &c, least);
            println!(
                "round {round} least {least}: beaten by {got:?} ({front} classes) in {:?}",
                t.elapsed()
            );
            if got.is_some() {
                c = z;
                continue 'rounds;
            }
        }
        break;
    }
}
