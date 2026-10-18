//! `cargo run --release --example solve -- AJ52 K76 [TARGET|mp|imps CONTRACT NEED]
//! [--vulnerable] [--budget SECS]
//! [--vacant EAST,WEST] [--east CARDS] [--west CARDS] [--length EAST,WEST]
//! [--hcp EAST,WEST] [--first N|S] [--entries NORTH,SOUTH] [--rounds N]
//! [--lead CARD] [--discard CARDS] [--ruffs N,E,S,W]`
//!
//! Prints the value of every goal (or just the one given) with its status,
//! and the line for the goal shown last.  `mp` and `imps` compare every
//! line (or the best mix of lines, if they beat each other in a cycle), the latter in CONTRACT (e.g. `3N`, `--vulnerable` or not) made
//! with NEED tricks in this suit.  `--budget` gives each goal at most
//! that many seconds; a goal out of time shows only its bound.  `--vacant`
//! sets the defenders' vacant places, `--east` and `--west` the cards known
//! to be with them, `--length` and `--hcp` (this suit only) ranges such as
//! `3`, `2-4`, `-2`, `3-` or `-` for any, e.g. `--length -,-1`.  `--first`
//! names the hand the lead starts in, `--entries` each hand's entries in
//! other suits, `-` for unlimited, e.g. `--entries -,1`.  `--rounds` stops
//! after that many rounds, `--lead` fixes declarer's first card, and
//! `--discard` takes declarer's cards out of play (not to the defenders).
//! `--ruffs` gives the rounds each seat may ruff once void, one side only.

use contract_bridge::{Holding, Rank, Seat};
use std::time::{Duration, Instant};
use suit_combination::{Comparison, Constraints, Error, Scoring, Solver};

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

const USAGE: &str = "usage: solve NORTH SOUTH [TARGET|mp|imps CONTRACT NEED] [--vulnerable] \
    [--budget SECS] \
    [--vacant EAST,WEST] [--east CARDS] [--west CARDS] [--length EAST,WEST] [--hcp EAST,WEST] \
    [--first N|S] [--entries NORTH,SOUTH] [--rounds N] [--lead CARD] [--discard CARDS] \
    [--ruffs N,E,S,W]";

/// Prints the lines of `c`, the best, and what each other gains over it
fn compared(c: &Comparison) {
    let per = |x: u64| x as f64 / c.total as f64;
    let unit = match c.scoring {
        Scoring::Matchpoints => "% of deals",
        Scoring::Imps { .. } => "IMPs per deal",
    };
    let scale = if unit.starts_with('%') { 100.0 } else { 1.0 };
    for (i, l) in c.lines.iter().enumerate() {
        let (seat, cards) = (l.seat, l.cards);
        let tricks: Vec<String> = l
            .tricks
            .iter()
            .map(|&w| format!("{:.2}", per(w) * 100.0))
            .collect();
        println!(
            "line {i}: lead {cards} from {seat:?}, % by tricks {}",
            tricks.join(" ")
        );
        if i > 0 {
            println!(
                "  vs line 0: wins {:.4}, loses {:.4} {unit}",
                per(c.gain(i, 0)) * scale,
                per(c.gain(0, i)) * scale
            );
        }
    }
    let sum: u64 = c.mix.iter().sum();
    let mix: Vec<String> = (c.best.iter())
        .map(|&i| format!("{i} ({:.2}%)", c.mix[i] as f64 / sum as f64 * 100.0))
        .collect();
    let mix = mix.join(", ");
    let slack = per(c.slack) / sum as f64 * scale;
    match (c.proven, c.open, c.best.len()) {
        (true, _, 1) => println!("best: line 0, and no line beats it"),
        (true, _, _) => println!(
            "no single best line; best: a mix of lines {mix}, and no line gains more than \
             {slack:.2e} {unit} over it"
        ),
        (_, true, 1) => println!("best found: line 0; interrupted before any proof"),
        (_, true, _) => println!("best mix found: lines {mix}; interrupted before any proof"),
        (_, _, 1) => println!("best found: line 0; unproven"),
        (_, _, _) => println!("best mix found: lines {mix}; unproven"),
    }
    println!("{}", c.lines[0].plan);
}

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
    let vulnerable = args
        .iter()
        .position(|a| a == "--vulnerable")
        .map(|i| args.remove(i));
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
    if let Some(v) = flag(&mut args, "--rounds") {
        constraints.rounds = v
            .parse()
            .unwrap_or_else(|_| fail("--rounds: a number expected"));
    }
    if let Some(v) = flag(&mut args, "--lead") {
        let rank = v.parse::<Rank>();
        constraints.lead = Some(rank.unwrap_or_else(|_| fail("--lead: a card expected, e.g. A")));
    }
    if let Some(v) = flag(&mut args, "--discard") {
        constraints.discards = holding("discard", &v);
    }
    if let Some(v) = flag(&mut args, "--ruffs") {
        let ruffs: Vec<u8> = v.split(',').filter_map(|n| n.parse().ok()).collect();
        let Ok(ruffs) = ruffs.try_into() else {
            fail("--ruffs: N,E,S,W expected, e.g. 0,1,0,1");
        };
        constraints.ruffs = ruffs;
    }
    let [north, south, rest @ ..] = args.as_slice() else {
        fail(USAGE);
    };
    let (north, south) = (holding("north", north), holding("south", south));
    let mut solver =
        Solver::with_constraints(north, south, constraints).unwrap_or_else(|e| fail(e));
    let total = solver.total();
    let d = constraints.discards;
    let max = ((north - d).len().max((south - d).len()) as u8).min(constraints.rounds);
    let scoring = match rest {
        [m] if m == "mp" => Some(Scoring::Matchpoints),
        [i, contract, need] if i == "imps" => Some(Scoring::Imps {
            contract: contract
                .parse()
                .unwrap_or_else(|_| fail(format!("contract: {contract}?"))),
            vulnerable: vulnerable.is_some(),
            need: need
                .parse()
                .unwrap_or_else(|_| fail(format!("need: {need}?"))),
        }),
        _ => None,
    };
    if let Some(scoring) = scoring {
        if let Some(budget) = budget {
            let deadline = Instant::now() + budget;
            solver.set_interrupt(move |_| Instant::now() >= deadline);
        }
        let c = solver.compare(scoring).unwrap_or_else(|e| fail(e));
        compared(&c);
        return;
    }
    let targets: Vec<u8> = match rest {
        [t] => vec![t.parse().unwrap_or_else(|_| fail(format!("target: {t}?")))],
        [] => (1..=max).collect(),
        _ => fail(USAGE),
    };
    let start = Instant::now();
    let results: Vec<_> = targets
        .iter()
        .map(|&target| {
            if let Some(budget) = budget {
                let deadline = Instant::now() + budget;
                solver.set_interrupt(move |_| Instant::now() >= deadline);
            }
            match solver.analyze(target) {
                Err(e @ (Error::BadTarget | Error::Unsupported)) => fail(e),
                r => (target, r),
            }
        })
        .collect();
    let elapsed = start.elapsed();
    let value = |score: u64| format!("{:.2}%", score as f64 / total as f64 * 100.0);
    println!("{north} opposite {south}");
    for (target, r) in &results {
        match r {
            Ok(a) => println!("{target} tricks: {}", value(a.score)),
            Err(Error::Interrupted { bound, .. }) => {
                println!("{target} tricks: interrupted (best <= {})", value(*bound));
            }
            Err(e) => fail(e),
        }
    }
    // Show the line for the last nontrivial target (every trick or no trick is no decision).
    if let Some(a) = results
        .iter()
        .rev()
        .filter_map(|(_, r)| r.as_ref().ok())
        .find(|a| a.score < total && a.score > 0)
    {
        println!("\nLine for {} tricks: {}", a.target, value(a.score));
        for l in &a.leads {
            println!("  lead {} from {:?}: {}", l.cards, l.seat, value(l.score));
        }
        print!("{}", a.plan);
    }
    eprintln!("({elapsed:?})");
}
