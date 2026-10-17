//! Suit-combination analysis and play through the public API.

use contract_bridge::{Holding, Seat};
use suit_combination::{
    Analysis, Constraints, Error, Game, Goal, Plan, Solver, TOTAL, analyze, analyze_all,
};

fn binom(n: u64, k: u64) -> u64 {
    (0..k).fold(1, |acc, i| acc * (n - i) / (i + 1))
}

fn score(north: &str, south: &str, goal: Goal) -> u64 {
    analyze(north.parse().unwrap(), south.parse().unwrap(), goal)
        .unwrap()
        .score
}

fn tricks(north: &str, south: &str, target: u8) -> u64 {
    score(north, south, Goal::Tricks { target })
}

fn lead_score(analysis: &Analysis, card: &str) -> u64 {
    let card: Holding = card.parse().unwrap();
    analysis
        .leads
        .iter()
        .find(|lead| lead.cards.to_bits() & card.to_bits() != 0)
        .unwrap()
        .score
}

#[test]
fn pavlicek_missing_the_jack() {
    // 4a00 #1: cash A or Q first; every layout can be picked up.
    assert_eq!(tricks("AQ954", "KT32", 5), TOTAL);
    // 4a00 #2: cash the K first; only East holding J876 (4-0) beats it.
    assert_eq!(tricks("AQ954", "K832", 5), TOTAL - binom(22, 13));
}

#[test]
fn pavlicek_missing_the_queen() {
    // https://www.rpbridge.net/4a00.htm #9: cash K to retain the finesse.
    // Only East holding all three missing cards (QT8) defeats five tricks.
    let analysis = analyze(
        "AJ976".parse().unwrap(),
        "K5432".parse().unwrap(),
        Goal::Tricks { target: 5 },
    )
    .unwrap();
    assert_eq!(analysis.score, TOTAL - binom(23, 13));
    assert_eq!(lead_score(&analysis, "K"), analysis.score);
    assert_eq!(lead_score(&analysis, "A"), TOTAL - 2 * binom(23, 13));
}

#[test]
fn pavlicek_unsupported_jack() {
    // https://www.rpbridge.net/4a00.htm #12: lead low, not the unsupported J.
    // Five tricks require West's K doubleton: four choices of companion.
    let analysis = analyze(
        "AQ654".parse().unwrap(),
        "J32".parse().unwrap(),
        Goal::Tricks { target: 5 },
    )
    .unwrap();
    assert_eq!(analysis.score, 4 * binom(21, 11));
    assert_eq!(lead_score(&analysis, "2"), analysis.score);
    assert_eq!(lead_score(&analysis, "J"), 0);
}

#[test]
fn todd_three_trick_safety_play() {
    // Robert S. Todd, "My Favorite Card Combination", Example 2:
    // https://www.advinbridge.com/this-week-in-bridge/608
    // Cash K, then lead toward A9: three tricks even against a 5-0 split.
    let analysis = analyze(
        "KJ43".parse().unwrap(),
        "A952".parse().unwrap(),
        Goal::Tricks { target: 3 },
    )
    .unwrap();
    assert_eq!(analysis.score, TOTAL);
    assert_eq!(lead_score(&analysis, "K"), TOTAL);
    assert!(lead_score(&analysis, "A") < TOTAL);
}

#[test]
fn splits() {
    // AKQxx opposite xx: five tricks need 3-3, four tricks need 4-2 or better.
    let three_three = 20 * binom(20, 10);
    let four_two = 30 * binom(20, 11);
    let five_one = 12 * binom(20, 12);
    let six_zero = 2 * binom(20, 13);
    assert_eq!(tricks("AKQ32", "54", 5), three_three);
    assert_eq!(tricks("AKQ32", "54", 4), three_three + four_two);
    assert_eq!(
        score("AKQ32", "54", Goal::Expected),
        5 * three_three + 4 * four_two + 3 * (five_one + six_zero)
    );
}

#[test]
fn stiff_king_offside() {
    // AQx opposite xx (8 missing): cash the A first, then lead toward the Q.
    // Beats K onside plus a singleton K offside — better than a plain finesse.
    let two = TOTAL / 2 + binom(18, 6);
    assert_eq!(tricks("AQ3", "42", 2), two);
    assert_eq!(score("AQ3", "42", Goal::Expected), 2 * two + (TOTAL - two));
}

#[test]
fn reading_group_problem_one() {
    // AJ52 opposite K76, three tricks: 3-3, West long 4-2, 2-4 with Q West,
    // 5-1 West long, Q singleton West, 6-0 West.  Derived by hand.
    let w = |west: u64| binom(20, 13 - west);
    let expected = 20 * w(3) + 15 * w(4) + 5 * w(2) + 6 * w(5) + w(1) + w(6);
    assert_eq!(tricks("AJ52", "K76", 3), expected);
}

#[test]
fn plays_out_against_the_layout() {
    let h = |s: &str| s.parse::<Holding>().unwrap();
    let goal = Goal::Tricks { target: 5 };
    for (west, want) in [("J98", 5), ("JT98", 4), ("JT9876", 3)] {
        let mut solver = Solver::new(h("AKQ32"), h("54")).unwrap();
        let mut game = Game::new(&mut solver, h(west), goal).unwrap();
        assert_eq!(game.moves(&mut solver), solver.analyze(goal).unwrap().leads);
        for rank in ['A', 'K', 'Q', '3', '2'] {
            if game.is_over() {
                break;
            }
            let weight = game.weight(&mut solver);
            assert!(game.moves(&mut solver).iter().all(|l| l.score <= weight));
            let plays = game
                .play(&mut solver, rank.to_string().parse().unwrap())
                .unwrap();
            assert_eq!(plays[0].seat, Seat::North);
            assert!(plays.len() >= 4, "{plays:?}");
        }
        assert!(game.is_over());
        assert_eq!(game.tricks_won(), want, "West {west}");
    }
    let mut solver = Solver::new(h("AKQ32"), h("54")).unwrap();
    assert_eq!(
        Game::new(&mut solver, h("A"), goal).unwrap_err(),
        Error::BadWest
    );
}

#[test]
fn expected_brackets_the_exact_search() {
    let h = |s: &str| s.parse::<Holding>().unwrap();
    // AQ3 opposite 42: one line is best for every target, so the bound is met.
    // 3 opposite AKT9: the line is the best, but no line is best for every
    // target.  A8 opposite JT62: the best average is on a line optimal for
    // no target, which the combined line misses.
    for (north, south, certified, exact) in [
        ("AQ3", "42", true, true),
        ("3", "AKT9", false, true),
        ("A8", "JT62", false, false),
    ] {
        let (n, s) = (h(north), h(south));
        let line = analyze(n, s, Goal::Expected).unwrap();
        let best = analyze(n, s, Goal::ExactExpected).unwrap();
        assert!(
            line.score <= best.score && best.score <= line.bound,
            "{north} {south}"
        );
        assert_eq!(line.score == line.bound, certified, "{north} {south}");
        assert_eq!(line.score == best.score, exact, "{north} {south}");
        assert_eq!(best.bound, best.score);
        assert_eq!(
            line.score,
            line.leads.iter().map(|l| l.score).max().unwrap()
        );
    }
}

#[test]
fn expected_line_plays_best_in_the_game() {
    // Following the recommended line, every card it plays is graded best.
    let h = |s: &str| s.parse::<Holding>().unwrap();
    for (north, south, wests) in [
        ("AQ3", "42", ["K", "KJ", "J9876"]),
        ("A8", "JT62", ["", "KQ", "Q9"]),
    ] {
        for west in wests {
            let mut solver = Solver::new(h(north), h(south)).unwrap();
            let analysis = solver.analyze(Goal::Expected).unwrap();
            let mut game = Game::new(&mut solver, h(west), Goal::Expected).unwrap();
            assert_eq!(game.moves(&mut solver), analysis.leads);
            let mut plan = &analysis.plan;
            while let Plan::Declarer { cards, then, .. } = plan {
                let moves = game.moves(&mut solver);
                let best = moves.iter().map(|l| l.score).max().unwrap();
                let chosen = moves.iter().find(|l| l.cards == *cards).unwrap();
                assert_eq!(chosen.score, best, "{north} {south} West {west}");
                let rank = cards.iter().next().unwrap();
                let plays = game.play(&mut solver, rank).unwrap();
                plan = then;
                // Follow the plan through the defenders' and forced cards.
                for p in &plays[1..] {
                    plan = match plan {
                        Plan::Defender { branches, .. } => {
                            &branches
                                .iter()
                                .find(|(labels, _)| labels.contains(&p.card))
                                .unwrap()
                                .1
                        }
                        Plan::Declarer { then, .. } => then,
                        _ => break,
                    };
                }
            }
        }
    }
}

#[test]
fn errors() {
    let h = |s: &str| s.parse::<Holding>().unwrap();
    let t = |target| Goal::Tricks { target };
    assert_eq!(analyze(h("AK"), h("A2"), t(1)).unwrap_err(), Error::Overlap);
    assert_eq!(
        analyze(h("AK"), h("32"), t(1)).unwrap_err(),
        Error::TooManyMissing
    );
    assert_eq!(
        analyze(h("AKQ"), h("32"), t(4)).unwrap_err(),
        Error::BadTarget
    );
    assert_eq!(analyze_all(h("AKQ"), h("32")).unwrap().len(), 4);
}

#[test]
fn restricted_choice_in_the_game() {
    // AKT98 / 5432, West J76: after the A, East drops the Q.  Equal cards
    // are interchangeable, so a layout with k of a class's m cards keeps
    // weight C(m, k) whichever card it showed: East's honour is twice as
    // likely singleton (Q or J) as QJ doubleton, and the finesse beats the
    // drop.  Counting only the deals holding the very Q seen would call
    // them equal (both 2 * C(22, 11) + C(22, 10) of 2 * C(22, 11) + 2 *
    // C(22, 10)).
    let h = |s: &str| s.parse::<Holding>().unwrap();
    let mut solver = Solver::new(h("AKT98"), h("5432")).unwrap();
    let mut game = Game::new(&mut solver, h("J76"), Goal::Tricks { target: 5 }).unwrap();
    let plays: Vec<_> = game
        .play(&mut solver, "A".parse().unwrap())
        .unwrap()
        .iter()
        .map(|p| (p.seat, p.card.map(|r| r.to_string())))
        .collect();
    let card = |s: &str| Some(s.to_string());
    assert_eq!(
        plays,
        [
            (Seat::North, card("A")),
            (Seat::East, card("Q")),
            (Seat::South, card("2")),
            (Seat::West, card("6")),
        ]
    );
    // East held one honour or both; West at least one of the 7 and 6
    let one = 4 * binom(22, 11) + 2 * binom(22, 10);
    let both = 2 * binom(22, 12) + binom(22, 11);
    assert_eq!(game.weight(&mut solver), one + both);
    let moves = game.moves(&mut solver);
    let by = |seat: Seat, cards: &str| {
        moves
            .iter()
            .find(|l| l.seat == seat && l.cards == h(cards))
            .unwrap()
            .score
    };
    // The finesse wins whenever East held one honour; the K wins against
    // QJ doubleton or West's Jx
    assert_eq!(by(Seat::South, "543"), one);
    assert_eq!(by(Seat::North, "K"), 5 * binom(22, 11));
}

#[test]
fn constraint_errors() {
    let h = |s: &str| s.parse::<Holding>().unwrap();
    let solver = |vacant, east: &str, west: &str| {
        let c = Constraints {
            vacant,
            known: [h(east), h(west)],
            ..Constraints::default()
        };
        Solver::with_constraints(h("AQ2"), h("543"), c)
    };
    let error = |r: Result<Solver, Error>| r.err();
    assert_eq!(
        error(solver([13, 13], "A", "")),
        Some(Error::BadConstraints)
    );
    assert_eq!(
        error(solver([13, 13], "K", "K")),
        Some(Error::BadConstraints)
    );
    assert_eq!(error(solver([14, 13], "", "")), Some(Error::BadConstraints));
    // Seven missing cards in six places, or two known cards in one place
    assert_eq!(error(solver([3, 3], "", "")), Some(Error::NoLayout));
    assert_eq!(error(solver([13, 1], "", "KJ")), Some(Error::NoLayout));

    let mut s = solver([13, 7], "", "K").unwrap();
    assert_eq!(s.analyze(Goal::Expected).unwrap_err(), Error::Unsupported);
    assert_eq!(
        s.analyze(Goal::ExactExpected).unwrap_err(),
        Error::Unsupported
    );
    // Every target, and no expected tricks
    assert_eq!(s.analyze_all().unwrap().len(), 3);
    let t = Goal::Tricks { target: 2 };
    assert_eq!(Game::new(&mut s, h("J"), t).unwrap_err(), Error::BadWest);
    // More cards than West's places
    let mut five = solver([13, 5], "", "").unwrap();
    assert_eq!(
        Game::new(&mut five, h("KJT987"), t).unwrap_err(),
        Error::BadWest
    );
    let game = Game::new(&mut s, h("KJ"), t).unwrap();
    assert_eq!(game.weight(&mut s), s.total());
}

#[test]
fn errors_of_play_constraints() {
    let h = |s: &str| s.parse::<Holding>().unwrap();
    let solver = |c| Solver::with_constraints(h("AQ"), h("432"), c);
    let d = Constraints::default();
    let error = |c| solver(c).err();
    // Both sides may ruff; a discard or a lead not declarer's; no round;
    // a lead from a hand with no way in
    assert_eq!(
        error(Constraints {
            ruffs: [1, 1, 0, 0],
            ..d
        }),
        Some(Error::Unsupported)
    );
    let bad = Some(Error::BadConstraints);
    assert_eq!(
        error(Constraints {
            discards: h("K"),
            ..d
        }),
        bad
    );
    assert_eq!(
        error(Constraints {
            lead: Some("K".parse().unwrap()),
            ..d
        }),
        bad
    );
    assert_eq!(error(Constraints { rounds: 0, ..d }), bad);
    let lead_a = Some("A".parse().unwrap());
    let south = Some(Seat::South);
    let c = Constraints {
        lead: lead_a,
        first: south,
        entries: [Some(0), None],
        ..d
    };
    assert_eq!(error(c), bad);
    assert_eq!(
        error(Constraints {
            first: Some(Seat::East),
            ..d
        }),
        bad
    );
    // Targets stop at the rounds
    let mut s = solver(Constraints { rounds: 1, ..d }).unwrap();
    assert_eq!(s.analyze_all().unwrap().len(), 1);
    let t = Goal::Tricks { target: 2 };
    assert_eq!(s.analyze(t).unwrap_err(), Error::BadTarget);
    // Games cannot show a ruff
    let mut s = solver(Constraints {
        ruffs: [0, 1, 0, 0],
        ..d
    })
    .unwrap();
    assert_eq!(
        Game::new(&mut s, h("K"), t).unwrap_err(),
        Error::Unsupported
    );
}

/// Declarer's ruffs take tricks double dummy does not see, so an
/// interrupted search must not bound by it
#[test]
fn interrupted_bound_counts_declarer_ruffs() {
    let h = |s: &str| s.parse::<Holding>().unwrap();
    let c = Constraints {
        ruffs: [1, 0, 0, 0],
        ..Constraints::default()
    };
    let goal = Goal::Tricks { target: 5 };
    let best = Solver::with_constraints(h("A2"), h("KQ543"), c)
        .unwrap()
        .analyze(goal)
        .unwrap()
        .score;
    let mut solver = Solver::with_constraints(h("A2"), h("KQ543"), c).unwrap();
    solver.set_poll_interval(1);
    solver.set_interrupt(|_| true);
    match solver.analyze(goal) {
        Err(Error::Interrupted { bound, .. }) => assert!(bound >= best, "{bound} < {best}"),
        r => panic!("{r:?}"),
    }
}
