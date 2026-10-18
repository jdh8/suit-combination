//! Benchmark corpus: generate it, or run it and report.
//!
//! ```sh
//! cargo run --release --example bench -- gen [SEED] > tests/fixtures/corpus.txt
//! cargo run --release --example bench -- run [CORPUS] [TIMEOUT_SECS] > results.tsv
//! ```
//!
//! `gen` samples one combination per stratum (missing cards 4..=12, North's
//! length, missing honours among AKQJ), then appends the known slow cases.
//! `run` solves every combination for each trick target, then compares its
//! lines at matchpoints (`mp`), one process per run so a timeout or crash
//! costs only that run.  An `mp` row's score is its line count and its
//! bound the lines in the best mix; its proof is `proven`, `mix` (a mix of
//! lines proven), `unproven` (a cycle no mix settled) or `interrupted`.
//! Each run gets the timeout as its engine budget, and is killed at twice
//! that.  Rows go to stdout as TSV, with `proof` saying how each run ended:
//! `proven`, `interrupted` (out of budget; `bound` still holds), `timeout`
//! (killed) or `error`.  The summary (p50/p95/max time of finished runs,
//! proof counts, memory) goes to stderr.

use contract_bridge::Holding;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use suit_combination::{Error, Scoring, Solver, TOTAL};

const SLOW: &[&str] = &[
    "A953 KJ8", "KJ84 T63", "KQ85 T73", "KJ85 T63", "KT85 J63", "KJ62 T84", "AQ95 T73", "A862 J94",
    "AQT8 642", "KT97 53", "A8 JT62", "KJ9753 -", "KJ963 5", "KJ63 95", "KJ863 -", "KJ86 3",
    "KJ965 3",
];

fn holding(s: &str) -> Holding {
    if s == "-" {
        Holding::EMPTY
    } else {
        s.parse().expect("holding")
    }
}

fn show(h: Holding) -> String {
    if h.is_empty() {
        "-".into()
    } else {
        h.to_string()
    }
}

fn generate(seed: u64) {
    let mut x = seed.max(1);
    let mut rand = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let honours: u16 = 0b111_1000_0000_0000; // AKQJ
    for missing in 4..=12u32 {
        let declarer = 13 - missing;
        for north_len in declarer.div_ceil(2)..=declarer {
            // Declarer holds at most `declarer` of the honours
            for h in 4u32.saturating_sub(declarer)..=missing.min(4) {
                loop {
                    let mut deck: Vec<u8> = (2..15).collect();
                    for i in (1..deck.len()).rev() {
                        deck.swap(i, (rand() % (i as u64 + 1)) as usize);
                    }
                    let bits = |r: &[u8]| r.iter().fold(0u16, |b, r| b | 1 << r);
                    let (ours, gone) = deck.split_at(declarer as usize);
                    if (bits(gone) & honours).count_ones() != h {
                        continue;
                    }
                    let (n, s) = ours.split_at(north_len as usize);
                    println!(
                        "{} {}",
                        show(Holding::from_bits_truncate(bits(n))),
                        show(Holding::from_bits_truncate(bits(s)))
                    );
                    break;
                }
            }
        }
    }
    for s in SLOW {
        println!("{s}");
    }
}

fn goals(north: Holding, south: Holding) -> Vec<String> {
    (1..=north.len().max(south.len()))
        .map(|t| format!("t{t}"))
        .chain(["mp".into()])
        .collect()
}

/// Solve one run within `budget` and print `score bound states widest ms
/// vmhwm_kb proof`
fn one(north: Holding, south: Holding, goal: &str, budget: Option<Duration>) {
    let start = Instant::now();
    let mut solver = Solver::new(north, south).expect("combination");
    if let Some(budget) = budget {
        solver.set_interrupt(move |_| start.elapsed() >= budget);
    }
    let (score, bound, proof) = if goal == "mp" {
        match solver.compare(Scoring::Matchpoints) {
            Ok(c) if c.open => (c.lines.len() as u64, c.best.len() as u64, "interrupted"),
            Ok(c) if !c.proven => (c.lines.len() as u64, c.best.len() as u64, "unproven"),
            Ok(c) if c.best.len() > 1 => (c.lines.len() as u64, c.best.len() as u64, "mix"),
            Ok(c) => (c.lines.len() as u64, 1, "proven"),
            Err(Error::Interrupted { .. }) => (0, 0, "interrupted"),
            Err(e) => panic!("{e}"),
        }
    } else {
        match solver.analyze(goal[1..].parse().expect("target")) {
            Ok(a) => (a.score, a.score, "proven"),
            Err(Error::Interrupted { bound, .. }) => (0, bound, "interrupted"),
            Err(e) => panic!("{e}"),
        }
    };
    let ms = start.elapsed().as_secs_f64() * 1e3;
    let (states, widest) = solver.stats();
    let hwm = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1).map(str::to_string))
        })
        .unwrap_or_default();
    println!("{score}\t{bound}\t{states}\t{widest}\t{ms:.3}\t{hwm}\t{proof}");
}

struct Row {
    north: String,
    south: String,
    goal: String,
    /// ok, interrupted, timeout or error
    status: String,
    /// proven, interrupted, timeout or error
    proof: String,
    score: u64,
    bound: u64,
    ms: f64,
    hwm_kb: u64,
    states: u64,
    widest: u64,
}

fn spawn(north: &str, south: &str, goal: &str, timeout: Duration) -> Row {
    let budget = timeout.as_secs_f64().to_string();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["one", north, south, goal, &budget])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let start = Instant::now();
    let mut row = Row {
        north: north.into(),
        south: south.into(),
        goal: goal.into(),
        status: "ok".into(),
        proof: String::new(),
        score: 0,
        bound: 0,
        ms: 0.0,
        hwm_kb: 0,
        states: 0,
        widest: 0,
    };
    let status = loop {
        if let Some(s) = child.try_wait().expect("wait") {
            break s;
        }
        // ponytail: one costly state can hold the engine past its budget
        // for seconds; past twice the budget the run counts as hung.
        if start.elapsed() > 2 * timeout {
            child.kill().ok();
            child.wait().ok();
            row.status = "timeout".into();
            row.proof = "timeout".into();
            row.ms = start.elapsed().as_secs_f64() * 1e3;
            return row;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let mut out = String::new();
    child.stdout.take().unwrap().read_to_string(&mut out).ok();
    let f: Vec<&str> = out.split_whitespace().collect();
    if !status.success() || f.len() < 7 {
        row.status = "error".into();
        row.proof = "error".into();
        row.ms = start.elapsed().as_secs_f64() * 1e3;
        return row;
    }
    row.score = f[0].parse().unwrap_or(0);
    row.bound = f[1].parse().unwrap_or(0);
    row.states = f[2].parse().unwrap_or(0);
    row.widest = f[3].parse().unwrap_or(0);
    row.ms = f[4].parse().unwrap_or(0.0);
    row.hwm_kb = f[5].parse().unwrap_or(0);
    row.proof = f[6].into();
    if row.proof == "interrupted" {
        row.status = "interrupted".into();
    }
    row
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn run(corpus: &str, timeout: Duration) {
    let text = std::fs::read_to_string(corpus).expect("corpus");
    println!("north\tsouth\tgoal\tstatus\tms\tvmhwm_kb\tscore\tbound\tstates\twidest\tproof");
    let mut rows = Vec::new();
    for line in text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let [n, s] = line.split_whitespace().collect::<Vec<_>>()[..] else {
            panic!("bad corpus line: {line}");
        };
        for goal in goals(holding(n), holding(s)) {
            let r = spawn(n, s, &goal, timeout);
            println!(
                "{}\t{}\t{}\t{}\t{:.3}\t{}\t{}\t{}\t{}\t{}\t{}",
                r.north,
                r.south,
                r.goal,
                r.status,
                r.ms,
                r.hwm_kb,
                r.score,
                r.bound,
                r.states,
                r.widest,
                r.proof
            );
            rows.push(r);
        }
    }
    let mut times: Vec<f64> = rows
        .iter()
        .filter(|r| r.status == "ok")
        .map(|r| r.ms)
        .collect();
    times.sort_by(f64::total_cmp);
    let count = |status: &str| rows.iter().filter(|r| r.status == status).count();
    let proof = |p: &str| rows.iter().filter(|r| r.proof == p).count();
    let slow: Vec<String> = rows
        .iter()
        .filter(|r| r.status != "ok" || r.ms > 1000.0)
        .map(|r| {
            let bound = if r.status == "interrupted" && r.goal != "mp" {
                format!(" (bound {:.4})", r.bound as f64 / TOTAL as f64)
            } else {
                String::new()
            };
            format!(
                "{} {} {} {} {:.0} ms{bound}",
                r.north, r.south, r.goal, r.status, r.ms
            )
        })
        .collect();
    eprintln!(
        "runs {} ok {} interrupted {} timeout {} error {}\n\
         proof: proven {} mix {} unproven {} interrupted {} timeout {} error {}\n\
         time ms p50 {:.1} p95 {:.1} max {:.1}\n\
         peak memory MB {:.0}\n\
         states max {} widest antichain {}\n\
         slow or failed:\n  {}",
        rows.len(),
        count("ok"),
        count("interrupted"),
        count("timeout"),
        count("error"),
        proof("proven"),
        proof("mix"),
        proof("unproven"),
        proof("interrupted"),
        proof("timeout"),
        proof("error"),
        percentile(&times, 0.5),
        percentile(&times, 0.95),
        percentile(&times, 1.0),
        rows.iter().map(|r| r.hwm_kb).max().unwrap_or(0) as f64 / 1024.0,
        rows.iter().map(|r| r.states).max().unwrap_or(0),
        rows.iter().map(|r| r.widest).max().unwrap_or(0),
        slow.join("\n  ")
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a[..] {
        ["gen"] => generate(1),
        ["gen", seed] => generate(seed.parse().expect("seed")),
        ["one", n, s, goal] => one(holding(n), holding(s), goal, None),
        ["one", n, s, goal, secs] => {
            let budget = Duration::from_secs_f64(secs.parse().expect("seconds"));
            one(holding(n), holding(s), goal, Some(budget));
        }
        ["run"] => run("tests/fixtures/corpus.txt", Duration::from_secs(30)),
        ["run", corpus] => run(corpus, Duration::from_secs(30)),
        ["run", corpus, secs] => run(corpus, Duration::from_secs(secs.parse().expect("seconds"))),
        _ => {
            eprintln!("usage: bench gen [SEED] | run [CORPUS] [TIMEOUT_SECS]");
            std::process::exit(2);
        }
    }
}
