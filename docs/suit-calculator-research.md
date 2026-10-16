# Suit-combination calculator research

Evidence checked: **2026-10-16**. Scope: bridge single-suit play with hidden defender cards; implementation language is not a ranking criterion. This is primary-source research, not a completed head-to-head benchmark. Product documentation establishes advertised capabilities, not independent correctness. No credible common benchmark establishes one worldwide winner.

The strongest targets found are **SuitPlay for analysis and scoring, SuitC for constraints, Bridge CCAnalyser for practical variant comparisons, BridgeTricks for interactive explanation, and FINESSE for published algorithmic work**. Pavlicek supplies useful reference examples and probability tools. These are complementary standards to match, rather than a defensible numerical league table.

## Leading comparators

| Comparator | Why include it | Evidence and limits |
|---|---|---|
| SuitPlay, Jeroen Warmerdam | Direct single-dummy analysis, falsecards, several scoring objectives, per-layout guarantees | Detailed first-party help; desktop version advertised as 2.1.2 (2011). No independently reproduced current benchmark here. |
| SuitC | Extensive restrictions, game-tree and layout details in a browser | Its guide discloses result-affecting memory pruning in difficult cases. |
| Bridge CCAnalyser, Lars Kappel Hansen | Target tricks, entries, prior information, side-by-side variants | Its guide explicitly does not guarantee finding the optimum. |
| BridgeTricks Suit Trainer / Solver | Card-by-card decisions, layout comparisons, shareable positions, training | Current exactness claims need verification; creator previously disclosed a capped search frontier. |
| FINESSE, Frank/Basin/Bundy | Published search method and readable explanations under the best-defense model | Historical research evaluation, not a currently verified downloadable product. |
| Richard Pavlicek resources | Human-readable regression examples and constrained layout probabilities | Distinguish instructional examples and probability calculator from an arbitrary optimal-play engine. |

The source-backed details below support this shortlist. “Not found” refers only to the bounded search and documents inspected, not proof that a feature or implementation does not exist.

## SuitPlay

SuitPlay's main help reports target-trick probabilities, expected tricks, interactive continuations, and per-layout trick guarantees against every legal defense, including falsecards. This makes it a particularly close semantic comparator for a pure strategy solver with omniscient defenders. It shows all allowed defender cards and highlights suitable declarer cards. [Main help](https://jeroenwarmerdam.pythonanywhere.com/suitplay/help/MainHelp.html)

Its advanced options include limited North entries, cards removed as planned discards/ruffs, and asymmetric vacant places. The entry rule charges an entry to lead from North unless North won the preceding suit trick. Its documented input restrictions require at least two defender cards and at least two cards in one declarer hand. [Combination/options help](https://jeroenwarmerdam.pythonanywhere.com/suitplay/help/Combo.html)

Matchpoint comparison is **pairwise probability of beating another line**, not expected tricks. IMP comparison uses expected IMP differences between lines after setting the contract and required suit tricks. The documentation's `AQ1098 / 5432` example distinguishes the maximum-expectation line from the matchpoint line. These are distinct parity features, not alternative labels for an EV calculation. [Comparison help](https://jeroenwarmerdam.pythonanywhere.com/suitplay/help/CompareLines.html)

The release history documents corrected play-display/secondary-objective bugs, a secondary preference for exploiting defensive errors, exportable analysis/payoff tables, and cases with no optimal MP/IMP line; `AJ98 / K32` is suggested for the MP case. [Release history](https://jeroenwarmerdam.pythonanywhere.com/suitplay/Versions.html)

The official site calls it freeware, advertises Windows version 2.1.2 released February 28, 2011, and offers download instructions through an automated email request. Its “flawless” wording and expert testimonials are promotional evidence, not a proof. No public solver source or open-source license was verified. No email was sent and no executable was tested in this research. [Official product page](https://jeroenwarmerdam.pythonanywhere.com/suitplay/home.html)

## SuitC

SuitC's browser interface exposes fixed defender cards, vacant places, opening lead, directed leads/entries, minimum defender lengths, ruffs and a trick-count limit. [Live interface](https://bridge.esmarkkappel.dk/suitc/main.html)

The guide describes target-trick/expectation summaries, branching continuations and results per defender distribution. Important qualifications: memory thresholds can eliminate search branches and affect some answers; at most ten plays are reported; compressed display trees may omit alternative declarer plays; specifying ruffs for both sides has unpredictable effects. Typical subsecond/few-second performance and difficult hours-long cases are author reports without a reproducible hardware/corpus benchmark. Its worked `KQT32 / 654` example gives 98.04% for two tricks, 89.57% for three, 55.39% for four and 3.43 expected tricks. [User guide, pp. 2–5](https://bridge.esmarkkappel.dk/suitc/SuitC%20User%20Guide_9.pdf)

The published feature matrix distinguishes SuitC from the CCAnalyser family and confirms its broader play/constraint controls. No publicly licensed solver source was found. [Author's feature matrix](https://bridge.esmarkkappel.dk/main/Tools%20and%20features5.pdf)

## Bridge CCAnalyser

The web product advertises target-trick analysis, alternative lines ranked by success probability, and pairwise variant comparisons. [Official web application](https://bridge.esmarkkappel.dk/main/main.html)

The 1.6 guide documents known side cards, restricted entries, initial leader and maximum defender HCP. It explains restricted-choice cases involving middle cards and includes falsecard examples. Forcing/avoiding lines are documented as Android-only; they can express a defender assumed not to make a particular falsecard. The Android app computes offline. Crucially, the algorithm section says large search trees must be pruned and the best answer is **not guaranteed**. This is therefore a valuable comparator, not an unquestionable correctness oracle. No reproducible performance benchmark or public solver-source license was verified. [Guide 1.6, especially pp. 3–4, 12–15, 20–26](https://bridge.esmarkkappel.dk/main/Guide%20to%20Bridge%20CCAnalyser%201.6.pdf)

## BridgeTricks

The current solver page advertises free arbitrary-combination solving without an account, success percentages per card, full contingent plans, layout-by-layout comparison and position URLs. Its description explicitly distinguishes a plan reacting to visible cards from one that can inspect hidden cards, and claims vacant-place probabilities and restricted choice. This is a strong usability target. Training also includes daily, practice and survival modes. [Solver and FAQ](https://bridgetricks.com/suit-trainer/solver)

There is a material historical qualification: in an August 9 creator comment, Alessandro Calmanovici said the exploration frontier was capped to meet runtime needs and discussed correcting continuation displays/recomputation. That is first-party evidence of a past approximation, **not proof the current implementation still uses it**. No public proof, reproducible full corpus, solver source or license resolving the current exactness claim was found. [Creator's comments](https://bridgewinners.com/bwcomments/user/alessandro-calmanovici-2-5umsser3e4/)

## FINESSE: research baseline

The AAAI 2000 paper uses pure declarer strategies against perfect-information defenders choosing second, propagates payoff vectors with dominance pruning, and combines seven bridge tactics with English explanations. It reports 665 combinations/1,561 objectives, about 0.64 seconds average total time on a 450 MHz SPARC-II, and errors in the reference answers. These historical results support studying tactical search and explanation generation; they do not establish a speed ranking against today's Rust implementation. A current runnable distribution and reusable implementation license were not verified. [Frank, Basin and Bundy, *Combining Knowledge and Search to Solve Single-Suit Bridge*](https://cdn.aaai.org/AAAI/2000/AAAI00-030.pdf)

## Pavlicek: reference and probability tooling

Pavlicek's 36-example lesson assumes adequate entries and unknown enemy hands, and warns that entry issues or extra information can change the play. It is a useful source of recognizable, explained regression cases; it is not an exhaustive arbitrary-combination calculator. [Card Combinations](https://www.rpbridge.net/4a00.htm)

His Card Combination Analyzer accepts defender length, suit HCP and vacant-space restrictions and offers text/CSV output. Its “free HCP in hand” adjustment is explicitly approximate. The inspected interface does not document automatic search for optimal contingent play; do not mistake constrained layout analysis for that capability. [Analyzer and help](https://www.rpbridge.net/cgi-bin/xcc1.pl) The calculator collection identifies its implementation language as Perl. [Calculator index](https://www.rpbridge.net/rpca.htm)

The site reserves redistribution rights. Link to examples and record independently derived results; do not assume permission to republish its entire instructional corpus. [Site notice](https://www.rpbridge.net/)

## Adjacent products and bounded Rust search

**BridgeDummy** explicitly labels its custom calculator double-dummy: it solves all layouts independently and aggregates their results, with an eight-missing-card limit. Those numbers are a ceiling on a hidden-information declarer strategy. It links a separate blind-play trainer, but the calculator is not a direct single-dummy correctness oracle. [Calculator FAQ](https://bridgedummy.com/card-combination-analyzer/)

**Quaestio** currently describes exhaustive hidden-information plans against omniscient, strategy-aware defense, with random equivalent cards, unlimited entries and no bidding information. It reports agreement within half a percentage point on 99/102 and 1,518/1,588 reference cases. An indexed earlier version retrieved during this search instead reported 93/102 and a defense rule preferring costless wins. This version discrepancy prevents a confident implementation/model classification without pinning the deployed code. Both counts are self-reported agreement, not certification; the tables and full differences were not supplied in the inspected page. [Current tool description](https://quaestio.app/en/maths/bridge-suit-combinations/)

The Rust search found this project's published `suit-combination` documentation, plus **Bridgitte**, explicitly a Rust double-dummy solver, and **dds-bridge**, a Rust interface to C++ DDS. Neither is a direct substitute for a single-dummy suit strategy. No separate mature Rust competitor with verified equivalent semantics surfaced; this is a bounded discovery result, not a claim of uniqueness. [This crate](https://docs.rs/suit-combination/latest/suit_combination/), [Bridgitte](https://github.com/mikea/bridgitte), [dds-bridge](https://github.com/jdh8/dds-bridge/blob/main/README.md)

Exact-name searches for “SuitGo” did not establish a credible bridge solver or primary product source. Do not add it to a competitor matrix as if verified.

## What an honest tie must demonstrate

This research suggests separate acceptance tracks: model-matched correctness against SuitPlay/independent exhaustive cases; constraints comparable to SuitC and CCAnalyser; interactive explanations comparable to BridgeTricks; and performance measured on a published, pinned corpus. Exactness, best-known value and proven bounds must remain distinguishable. Falsecards, restricted choice, scoring goals, entry semantics and permitted priors must match before treating differences as defects. Published tables and competitor output should seed investigations, not silently override a proof or independently checked strategy.

The [project comparison and implementation plan](parity-plan.md) records local measurements separately. This note did not run third-party solver binaries, audit their implementations, reproduce their benchmark claims or establish modern cross-product speed parity.
