//! Ranking metrics for a method's scores against a truth: AUROC and average
//! precision over a universe of items, each scored or not, and the counts a
//! reader needs to judge them.
//!
//! Scores are cleaned before they are ranked ([`clean`]):
//! - `-0.0` becomes `+0.0`, so the two are one tie;
//! - NaN, a missing score (`None`) and `-inf` are unscored: one block, tied,
//!   below every finite score;
//! - `+inf` stays: one tie group above every finite score.
//!
//! An unscored item stays in the universe. A positive the method did not
//! score counts against it, at the bottom of the ranking; it is never
//! dropped.

use std::cmp::Ordering;

/// What a ranking holds, beside its metrics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Counts {
    pub n_pos: usize,
    pub n_neg: usize,
    /// Every item, scored or not.
    pub n_universe: usize,
    /// `n_pos / n_universe`, the AP of a ranking that ties every item (a
    /// random ranking's expected AP tends to it as the universe grows); NaN
    /// for an empty universe.
    pub pos_rate: f64,
    /// The share of scored items in tie groups of more than one, the
    /// unscored block left out; NaN when nothing is scored.
    pub tied_share: f64,
    /// Items without a score (NaN, missing or `-inf`).
    pub unscored: usize,
    /// Positives among them.
    pub unscored_pos: usize,
    /// Items scored `+inf`, the top tie group.
    pub n_inf: usize,
}

/// A score made ready to rank: `None` when unscored (missing, NaN or
/// `-inf`), `+0.0` for `-0.0`, any other value as it is.
pub fn clean(score: Option<f64>) -> Option<f64> {
    match score {
        Some(x) if x.is_nan() || x == f64::NEG_INFINITY => None,
        // A float pattern matches by ==, and -0.0 == 0.0: both become +0.0.
        Some(0.0) => Some(0.0),
        s => s,
    }
}

/// The area under the ROC curve: the chance that a random positive ranks
/// above a random negative, ties counting a half (the Mann-Whitney U over
/// `n_pos * n_neg`, from midranks over every item, the unscored block one
/// tie). NaN without a positive or a negative.
///
/// # Panics
///
/// When `scores` and `labels` differ in length.
pub fn auroc(scores: &[Option<f64>], labels: &[bool]) -> f64 {
    auroc_of(&ranked(scores, labels))
}

/// Average precision, the step-wise area under the precision-recall curve:
/// over the distinct thresholds `t_k` from high to low, each tie group one
/// threshold and the unscored block the last, `sum (R_k - R_{k-1}) P_k`
/// with `P_k` and `R_k` counted over the items at or above `t_k`. No
/// interpolation, no trapezoid. NaN without a positive or a negative.
///
/// # Panics
///
/// When `scores` and `labels` differ in length.
pub fn average_precision(scores: &[Option<f64>], labels: &[bool]) -> f64 {
    ap_of(&ranked(scores, labels))
}

/// The counts beside the metrics (see [`Counts`]).
///
/// # Panics
///
/// When `scores` and `labels` differ in length.
pub fn counts(scores: &[Option<f64>], labels: &[bool]) -> Counts {
    counts_of(&ranked(scores, labels))
}

/// Items tied after cleaning: how many, how many positive, and their
/// cleaned score (`None` for the unscored block).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Group {
    items: usize,
    pos: usize,
    score: Option<f64>,
}

/// The ranking's tie groups, from the highest score down.
fn ranked(scores: &[Option<f64>], labels: &[bool]) -> Vec<Group> {
    assert_eq!(scores.len(), labels.len(), "one label per score");
    let cleaned: Vec<_> = scores.iter().map(|&s| clean(s)).collect();
    groups(&cleaned, labels)
}

/// The tie groups of cleaned scores, from the highest down.
fn groups(cleaned: &[Option<f64>], labels: &[bool]) -> Vec<Group> {
    let mut items: Vec<_> = cleaned
        .iter()
        .copied()
        .zip(labels.iter().copied())
        .collect();
    items.sort_unstable_by(|a, b| order(b.0, a.0));
    items
        .chunk_by(|a, b| order(a.0, b.0).is_eq())
        .map(|tied| Group {
            items: tied.len(),
            pos: tied.iter().filter(|(_, pos)| *pos).count(),
            score: tied[0].0,
        })
        .collect()
}

/// Cleaned scores in rank order, unscored lowest. Cleaned values hold no
/// NaN and no -0.0, so `total_cmp` on them is the IEEE order (`partial_cmp`)
/// made total.
fn order(a: Option<f64>, b: Option<f64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        _ => a.is_some().cmp(&b.is_some()),
    }
}

/// The positives and negatives over the groups.
fn labelled(groups: &[Group]) -> (usize, usize) {
    let pos = groups.iter().map(|g| g.pos).sum::<usize>();
    (pos, groups.iter().map(|g| g.items).sum::<usize>() - pos)
}

fn auroc_of(groups: &[Group]) -> f64 {
    let (n_pos, n_neg) = labelled(groups);
    if n_pos == 0 || n_neg == 0 {
        return f64::NAN;
    }
    // From the bottom: a group with `below` items under it holds ranks
    // below + 1 ..= below + items, whose mean each of its items takes.
    let mut below = n_pos + n_neg;
    let mut rank_sum = 0.0;
    for g in groups {
        below -= g.items;
        rank_sum += g.pos as f64 * (below as f64 + (g.items as f64 + 1.0) / 2.0);
    }
    let (n_pos, n_neg) = (n_pos as f64, n_neg as f64);
    (rank_sum - n_pos * (n_pos + 1.0) / 2.0) / (n_pos * n_neg)
}

fn ap_of(groups: &[Group]) -> f64 {
    let (n_pos, n_neg) = labelled(groups);
    if n_pos == 0 || n_neg == 0 {
        return f64::NAN;
    }
    // Each group adds g.pos / n_pos to the recall, at the precision of
    // every item down to it.
    let (mut tp, mut seen, mut sum) = (0, 0, 0.0);
    for g in groups {
        tp += g.pos;
        seen += g.items;
        sum += g.pos as f64 * tp as f64 / seen as f64;
    }
    sum / n_pos as f64
}

fn counts_of(groups: &[Group]) -> Counts {
    let (n_pos, n_neg) = labelled(groups);
    let n_universe = n_pos + n_neg;
    let unscored = groups.iter().find(|g| g.score.is_none());
    let unscored_items = unscored.map_or(0, |g| g.items);
    let tied = groups
        .iter()
        .filter(|g| g.score.is_some() && g.items > 1)
        .map(|g| g.items)
        .sum::<usize>();
    Counts {
        n_pos,
        n_neg,
        n_universe,
        pos_rate: n_pos as f64 / n_universe as f64,
        tied_share: tied as f64 / (n_universe - unscored_items) as f64,
        unscored: unscored_items,
        unscored_pos: unscored.map_or(0, |g| g.pos),
        n_inf: groups
            .iter()
            .find(|g| g.score == Some(f64::INFINITY))
            .map_or(0, |g| g.items),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ranking with the metrics worked out by hand.
    struct Case {
        scores: Vec<Option<f64>>,
        labels: Vec<bool>,
        auroc: f64,
        ap: f64,
    }

    fn case(scores: &[f64], labels: &[u8], auroc: f64, ap: f64) -> Case {
        Case {
            scores: scores.iter().map(|&x| Some(x)).collect(),
            labels: labels.iter().map(|&l| l == 1).collect(),
            auroc,
            ap,
        }
    }

    /// Equal to 1e-12, or both NaN.
    fn near(got: f64, want: f64) -> bool {
        (got.is_nan() && want.is_nan()) || (got - want).abs() <= 1e-12
    }

    fn holds(c: &Case) {
        let (roc, ap) = (
            auroc(&c.scores, &c.labels),
            average_precision(&c.scores, &c.labels),
        );
        assert!(near(roc, c.auroc), "AUROC {roc}, want {}", c.auroc);
        assert!(near(ap, c.ap), "AP {ap}, want {}", c.ap);
    }

    // P P N in score order: every positive above the negative, so U = 2 of
    // 2. AP: P=1 at R=1/2, P=1 at R=1.
    fn perfect() -> Case {
        case(&[3.0, 2.0, 1.0], &[1, 1, 0], 1.0, 1.0)
    }

    // N P P in score order: U = 0. AP: the top threshold (3) has TP 0 and
    // adds no recall; at 2, TP 1 of 2 seen (P 1/2, R +1/2); at 1, TP 2 of 3
    // (P 2/3, R +1/2): 1/4 + 1/3 = 7/12.
    fn reversed() -> Case {
        case(&[1.0, 2.0, 3.0], &[1, 1, 0], 0.0, 7.0 / 12.0)
    }

    // One tie group: every pair ties, U = 4 halves of 4. AP: one threshold,
    // P 2/4 at R 1.
    fn all_tied() -> Case {
        case(&[1.0; 4], &[1, 0, 1, 0], 0.5, 0.5)
    }

    // P at 0.9 beats both negatives (2); P at 0.5 ties N at 0.5 (1/2) and
    // beats N at 0.1 (1): U = 3.5 of 4 = 7/8. AP: at 0.9, P 1 at R 1/2; at
    // 0.5 (one threshold), TP 2 of 3 (P 2/3, R +1/2): 1/2 + 1/3 = 5/6.
    fn mixed_ties() -> Case {
        case(&[0.9, 0.5, 0.5, 0.1], &[1, 1, 0, 0], 7.0 / 8.0, 5.0 / 6.0)
    }

    // The same, the tied pair given the other way round.
    fn mixed_ties_swapped() -> Case {
        case(&[0.9, 0.5, 0.5, 0.1], &[1, 0, 1, 0], 7.0 / 8.0, 5.0 / 6.0)
    }

    // P 0.9, P unscored, N 0.5: the scored positive beats the negative (1),
    // the unscored one sits below it (0): U = 1 of 2. AP: at 0.9, P 1 at R
    // 1/2; at 0.5, no recall; the unscored block, TP 2 of 3 (P 2/3, R +1/2):
    // 1/2 + 1/3 = 5/6. Dropping the unscored positive would give 1 and 1.
    fn unscored_positive() -> Case {
        Case {
            scores: vec![Some(0.9), None, Some(0.5)],
            labels: vec![true, true, false],
            auroc: 0.5,
            ap: 5.0 / 6.0,
        }
    }

    // A universe of 1000 with 10 positives; the method scores 2 items, both
    // positive. AP: at the first score, P 1/1 at R 1/10; at the second, P
    // 2/2 at R 2/10; the unscored block of 998 (8 positives) is the last
    // threshold, where all 1000 items are at or above it: P 10/1000, R
    // +8/10. AP = 0.1 + 0.1 + 0.8 * 10/1000 = 0.208. AUROC: the two scored
    // positives beat all 990 negatives (1980); the 8 unscored tie all 990
    // (8 * 990 / 2 = 3960): U = 5940 of 9900 = 0.6. The 8 unscored
    // positives are items 50, 175, ..., 925.
    fn few_links() -> Case {
        let mut scores = vec![None; 1000];
        scores[0] = Some(2.0);
        scores[1] = Some(1.0);
        let labels = (0..1000).map(|i| i < 2 || i % 125 == 50).collect();
        Case {
            scores,
            labels,
            auroc: 0.6,
            ap: 0.208,
        }
    }

    // N +0.0, P -0.0: one tie, so U = 1/2 of 1 and AP = P 1/2 at R 1. Were
    // -0.0 below +0.0, AUROC would be 0.
    fn signed_zero() -> Case {
        case(&[0.0, -0.0], &[0, 1], 0.5, 0.5)
    }

    // N NaN, P 0.1: NaN is unscored, below the positive: 1 and 1. Were NaN on
    // top, AUROC 0 and AP 1/2.
    fn nan_below_positive() -> Case {
        case(&[f64::NAN, 0.1], &[0, 1], 1.0, 1.0)
    }

    // P NaN, N 0.1: the negative is above: U = 0. AP: at 0.1 no recall; the
    // unscored block, P 1/2 at R 1.
    fn nan_positive() -> Case {
        case(&[f64::NAN, 0.1], &[1, 0], 0.0, 0.5)
    }

    // P NaN, N missing, P -inf, N 1.0: the first three are one unscored
    // block under N 1.0. Each positive loses to N 1.0 and ties the unscored
    // negative: U = 2 halves of 4 = 1/4. AP: at 1.0 no recall; the block,
    // TP 2 of 4 (P 1/2, R 1): 1/2.
    fn unscored_block() -> Case {
        Case {
            scores: vec![Some(f64::NAN), None, Some(f64::NEG_INFINITY), Some(1.0)],
            labels: vec![true, false, true, false],
            auroc: 0.25,
            ap: 0.5,
        }
    }

    // P +inf, N +inf, P 5: +inf is one tie group on top. P +inf ties N +inf
    // (1/2); P 5 loses to it (0): U = 1/2 of 2 = 1/4. AP: at +inf, TP 1 of
    // 2 (P 1/2, R 1/2); at 5, TP 2 of 3 (P 2/3, R +1/2): 1/4 + 1/3 = 7/12.
    fn inf_on_top() -> Case {
        case(
            &[f64::INFINITY, f64::INFINITY, 5.0],
            &[1, 0, 1],
            0.25,
            7.0 / 12.0,
        )
    }

    fn no_positives() -> Case {
        case(&[1.0, 2.0], &[0, 0], f64::NAN, f64::NAN)
    }

    fn no_negatives() -> Case {
        case(&[1.0, 2.0], &[1, 1], f64::NAN, f64::NAN)
    }

    const CASES: [fn() -> Case; 14] = [
        perfect,
        reversed,
        all_tied,
        mixed_ties,
        mixed_ties_swapped,
        unscored_positive,
        few_links,
        signed_zero,
        nan_below_positive,
        nan_positive,
        unscored_block,
        inf_on_top,
        no_positives,
        no_negatives,
    ];

    #[test]
    fn a_perfect_ranking_scores_one() {
        holds(&perfect());
    }

    #[test]
    fn a_reversed_ranking_scores_zero_and_seven_twelfths() {
        holds(&reversed());
    }

    #[test]
    fn all_tied_scores_one_half() {
        holds(&all_tied());
    }

    #[test]
    fn a_tie_is_one_threshold_whatever_its_order() {
        holds(&mixed_ties());
        holds(&mixed_ties_swapped());
    }

    #[test]
    fn an_unscored_positive_counts_at_the_bottom() {
        holds(&unscored_positive());
    }

    #[test]
    fn few_links_scored_give_ap_0_208() {
        holds(&few_links());
    }

    #[test]
    fn minus_zero_and_zero_are_one_tie() {
        holds(&signed_zero());
        holds(&case(&[-0.0, 0.0], &[0, 1], 0.5, 0.5));
    }

    #[test]
    fn nan_never_ranks_top() {
        holds(&nan_below_positive());
        holds(&nan_positive());
    }

    #[test]
    fn nan_missing_and_minus_inf_are_one_block() {
        holds(&unscored_block());
    }

    #[test]
    fn inf_is_one_tie_group_on_top() {
        holds(&inf_on_top());
    }

    #[test]
    fn without_positives_or_negatives_both_are_nan() {
        holds(&no_positives());
        holds(&no_negatives());
        holds(&case(&[], &[], f64::NAN, f64::NAN));
    }

    #[test]
    fn cleaning_merges_the_zeros_and_the_unscored() {
        assert_eq!(clean(Some(-0.0)).map(f64::to_bits), Some(0.0f64.to_bits()));
        assert_eq!(clean(Some(f64::NAN)), None);
        assert_eq!(clean(Some(-f64::NAN)), None);
        assert_eq!(clean(Some(f64::from_bits(0x7ff0_0000_0000_0001))), None);
        assert_eq!(clean(None), None);
        assert_eq!(clean(Some(f64::NEG_INFINITY)), None);
        assert_eq!(clean(Some(f64::INFINITY)), Some(f64::INFINITY));
        assert_eq!(clean(Some(-2.5)), Some(-2.5));
    }

    // 12 items. Scored: 0.9, 0.5 x2, +inf x3, 0.1, -0.0 and +0.0 (one tie):
    // 9, of which 2 + 3 + 2 = 7 are in ties of more than one. Unscored:
    // missing, NaN, -inf: 3, positives at the missing and the -inf.
    fn counted() -> (Vec<Option<f64>>, Vec<bool>, Counts) {
        let inf = f64::INFINITY;
        let scores = vec![
            Some(0.9),
            Some(0.5),
            Some(0.5),
            Some(inf),
            Some(inf),
            Some(inf),
            Some(0.1),
            None,
            Some(f64::NAN),
            Some(-inf),
            Some(-0.0),
            Some(0.0),
        ];
        let labels = [1, 0, 1, 1, 0, 0, 0, 1, 0, 1, 0, 0];
        let want = Counts {
            n_pos: 5,
            n_neg: 7,
            n_universe: 12,
            pos_rate: 5.0 / 12.0,
            tied_share: 7.0 / 9.0,
            unscored: 3,
            unscored_pos: 2,
            n_inf: 3,
        };
        (scores, labels.iter().map(|&l| l == 1).collect(), want)
    }

    #[test]
    fn counts_leave_the_unscored_block_out_of_the_ties() {
        let (scores, labels, want) = counted();
        assert_eq!(counts(&scores, &labels), want);
        let c = few_links();
        let got = counts(&c.scores, &c.labels);
        let want = Counts {
            n_pos: 10,
            n_neg: 990,
            n_universe: 1000,
            pos_rate: 0.01,
            tied_share: 0.0,
            unscored: 998,
            unscored_pos: 8,
            n_inf: 0,
        };
        assert_eq!(got, want);
        let none = counts(&[None, None], &[true, false]);
        assert!(none.tied_share.is_nan() && none.unscored == 2, "{none:?}");
        assert!(counts(&[], &[]).pos_rate.is_nan());
    }

    /// The metrics, and the counts, of one way of ranking.
    type Metrics = fn(&[Option<f64>], &[bool]) -> (f64, f64, Counts);

    fn real(scores: &[Option<f64>], labels: &[bool]) -> (f64, f64, Counts) {
        (
            auroc(scores, labels),
            average_precision(scores, labels),
            counts(scores, labels),
        )
    }

    fn of_groups(g: &[Group]) -> (f64, f64, Counts) {
        (auroc_of(g), ap_of(g), counts_of(g))
    }

    /// Cleaned and sorted from the top down, each item its own group: ties
    /// broken by input order.
    fn ties_by_input_order(scores: &[Option<f64>], labels: &[bool]) -> (f64, f64, Counts) {
        let mut items: Vec<_> = scores.iter().map(|&s| clean(s)).zip(labels).collect();
        items.sort_by(|a, b| order(b.0, a.0));
        let singles: Vec<Group> = items
            .iter()
            .map(|&(score, &pos)| Group {
                items: 1,
                pos: pos as usize,
                score,
            })
            .collect();
        of_groups(&singles)
    }

    /// AUROC from ordinal ranks, 1 to n in ascending order, ties in input
    /// order.
    fn ordinal_ranks(scores: &[Option<f64>], labels: &[bool]) -> (f64, f64, Counts) {
        let (_, ap, c) = real(scores, labels);
        let mut items: Vec<_> = scores.iter().map(|&s| clean(s)).zip(labels).collect();
        items.sort_by(|a, b| order(a.0, b.0));
        let (np, nn) = (c.n_pos as f64, c.n_neg as f64);
        let rank_sum: f64 = (1..)
            .zip(&items)
            .filter(|(_, (_, &pos))| pos)
            .map(|(r, _)| r as f64)
            .sum();
        let roc = (rank_sum - np * (np + 1.0) / 2.0) / (np * nn);
        (if np * nn == 0.0 { f64::NAN } else { roc }, ap, c)
    }

    /// AP as the trapezoid under the PR points, from (0, 1).
    fn pr_trapezoid(scores: &[Option<f64>], labels: &[bool]) -> (f64, f64, Counts) {
        let (roc, ap, c) = real(scores, labels);
        let (mut tp, mut seen, mut area) = (0, 0, 0.0);
        let (mut r0, mut p0) = (0.0, 1.0);
        for g in ranked(scores, labels) {
            tp += g.pos;
            seen += g.items;
            let (r, p) = (tp as f64 / c.n_pos as f64, tp as f64 / seen as f64);
            area += (r - r0) * (p + p0) / 2.0;
            (r0, p0) = (r, p);
        }
        (roc, if ap.is_nan() { ap } else { area }, c)
    }

    /// Unscored items left out of the ranking.
    fn unscored_dropped(scores: &[Option<f64>], labels: &[bool]) -> (f64, f64, Counts) {
        let (s, l): (Vec<_>, Vec<_>) = scores
            .iter()
            .zip(labels)
            .filter(|(s, _)| clean(**s).is_some())
            .unzip();
        real(&s, &l)
    }

    /// Cleaned but for -0.0, which stays below +0.0.
    fn minus_zero_kept(scores: &[Option<f64>], labels: &[bool]) -> (f64, f64, Counts) {
        let kept: Vec<_> = scores
            .iter()
            .map(|&s| match s {
                Some(x @ 0.0) => Some(x),
                s => clean(s),
            })
            .collect();
        of_groups(&groups(&kept, labels))
    }

    /// Cleaned but for NaN, which stays and sorts above everything.
    fn nan_on_top(scores: &[Option<f64>], labels: &[bool]) -> (f64, f64, Counts) {
        let kept: Vec<_> = scores
            .iter()
            .map(|&s| match s {
                Some(x) if x.is_nan() => Some(f64::NAN),
                s => clean(s),
            })
            .collect();
        of_groups(&groups(&kept, labels))
    }

    /// The tie share with the unscored block counted as a tie.
    fn unscored_counted_as_tied(scores: &[Option<f64>], labels: &[bool]) -> (f64, f64, Counts) {
        let (roc, ap, mut c) = real(scores, labels);
        let tied: usize = ranked(scores, labels)
            .iter()
            .filter(|g| g.items > 1)
            .map(|g| g.items)
            .sum();
        c.tied_share = tied as f64 / c.n_universe as f64;
        (roc, ap, c)
    }

    /// AP with no guard, as the earlier implementation has it: without
    /// negatives every precision is 1, so it scores 1 (without positives,
    /// 0 / 0 still gives NaN).
    fn ap_without_negatives(scores: &[Option<f64>], labels: &[bool]) -> (f64, f64, Counts) {
        let (roc, _, c) = real(scores, labels);
        let (mut tp, mut seen, mut sum) = (0, 0, 0.0);
        for g in ranked(scores, labels) {
            tp += g.pos;
            seen += g.items;
            sum += g.pos as f64 * tp as f64 / seen as f64;
        }
        (roc, sum / c.n_pos as f64, c)
    }

    /// `n_inf` counted on the raw scores as every infinity, `-inf` too.
    fn minus_inf_counted_as_inf(scores: &[Option<f64>], labels: &[bool]) -> (f64, f64, Counts) {
        let (roc, ap, mut c) = real(scores, labels);
        c.n_inf = scores.iter().flatten().filter(|x| x.is_infinite()).count();
        (roc, ap, c)
    }

    const MUTANTS: [(&str, Metrics); 9] = [
        ("ties broken by input order", ties_by_input_order),
        ("ordinal ranks", ordinal_ranks),
        ("PR trapezoid", pr_trapezoid),
        ("unscored items dropped", unscored_dropped),
        ("-0.0 not cleaned", minus_zero_kept),
        ("NaN sorted above everything", nan_on_top),
        ("unscored block counted as tied", unscored_counted_as_tied),
        ("AP not NaN without negatives", ap_without_negatives),
        ("-inf counted in n_inf", minus_inf_counted_as_inf),
    ];

    /// Whether `metrics` gets every case and the counts right.
    fn passes(metrics: Metrics) -> bool {
        let cases = CASES.iter().all(|c| {
            let c = c();
            let (roc, ap, _) = metrics(&c.scores, &c.labels);
            near(roc, c.auroc) && near(ap, c.ap)
        });
        let (scores, labels, want) = counted();
        cases && metrics(&scores, &labels).2 == want
    }

    #[test]
    fn every_broken_variant_fails_a_case() {
        assert!(passes(real));
        for (name, broken) in MUTANTS {
            assert!(!passes(broken), "the variant with {name} passes every case");
        }
    }

    /// An item's place by class: 0 unscored, then 0.0, 1.0 and +inf; as a
    /// score, written in one of the forms of its class.
    fn raw(class: usize, form: usize) -> Option<f64> {
        match class {
            0 => [
                None,
                Some(f64::NAN),
                Some(f64::NEG_INFINITY),
                Some(-f64::NAN),
            ][form % 4],
            1 => [Some(-0.0), Some(0.0)][form % 2],
            2 => Some(1.0),
            _ => Some(f64::INFINITY),
        }
    }

    /// AUROC as wins and half ties over every positive and negative pair.
    fn pairs_auroc(class: &[usize], labels: &[bool]) -> f64 {
        let (mut halves, mut pairs) = (0, 0);
        for i in 0..class.len() {
            for j in 0..class.len() {
                if labels[i] && !labels[j] {
                    pairs += 1;
                    halves += match class[i].cmp(&class[j]) {
                        Ordering::Greater => 2,
                        Ordering::Equal => 1,
                        Ordering::Less => 0,
                    };
                }
            }
        }
        halves as f64 / 2.0 / pairs as f64
    }

    /// AP by sweeping each distinct class as a threshold, high to low,
    /// counting the items at or above it.
    fn sweep_ap(class: &[usize], labels: &[bool]) -> f64 {
        let n_pos = labels.iter().filter(|&&l| l).count();
        if n_pos == 0 || n_pos == labels.len() {
            return f64::NAN;
        }
        let (mut ap, mut tp_before) = (0.0, 0);
        for t in (0..4).rev() {
            let (mut tp, mut fp, mut at_t) = (0, 0, false);
            for i in 0..class.len() {
                at_t |= class[i] == t;
                if class[i] >= t {
                    (tp, fp) = if labels[i] {
                        (tp + 1, fp)
                    } else {
                        (tp, fp + 1)
                    };
                }
            }
            if at_t {
                ap += (tp - tp_before) as f64 / n_pos as f64 * tp as f64 / (tp + fp) as f64;
                tp_before = tp;
            }
        }
        ap
    }

    /// Every labelled vector of up to `longest` items over the first
    /// `classes` classes, against the brute-force metrics, split over the
    /// cores. (The public functions are these over [`ranked`]; ranking once
    /// keeps it quick.)
    fn brute_force(classes: usize, longest: u32) {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        for n in 0..=longest {
            let codes = (2 * classes).pow(n);
            std::thread::scope(|s| {
                for core in 0..cores {
                    s.spawn(move || {
                        for code in (core..codes).step_by(cores) {
                            vector_holds(classes, n as usize, code);
                        }
                    });
                }
            });
        }
    }

    /// Vector number `code` of `n` items: each item's class and label are
    /// its digit in base `2 * classes`.
    fn vector_holds(classes: usize, n: usize, code: usize) {
        let mut class = Vec::with_capacity(n);
        let mut labels = Vec::with_capacity(n);
        let mut scores = Vec::with_capacity(n);
        let mut rest = code;
        for i in 0..n {
            class.push(rest % classes);
            labels.push(rest / classes % 2 == 1);
            scores.push(raw(rest % classes, i + code));
            rest /= 2 * classes;
        }
        let g = ranked(&scores, &labels);
        let (roc, ap) = (auroc_of(&g), ap_of(&g));
        assert!(
            near(roc, pairs_auroc(&class, &labels)),
            "{scores:?} {labels:?}"
        );
        assert!(near(ap, sweep_ap(&class, &labels)), "{scores:?} {labels:?}");
    }

    #[test]
    fn every_short_vector_matches_the_brute_force() {
        brute_force(3, 8);
        brute_force(4, 6);
    }

    /// A small linear congruential generator, for vectors that repeat.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) % n as u64) as usize
        }
    }

    fn same(a: f64, b: f64) -> bool {
        a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
    }

    #[test]
    fn the_input_order_changes_nothing() {
        let mut rng = Lcg(7);
        for _ in 0..500 {
            let n = 1 + rng.below(40);
            let mut items: Vec<(Option<f64>, bool)> = (0..n)
                .map(|i| (raw(rng.below(4), i), rng.below(3) == 0))
                .collect();
            let split = |items: &[(Option<f64>, bool)]| -> (Vec<_>, Vec<_>) {
                items.iter().copied().unzip()
            };
            let (s, l) = split(&items);
            let before = real(&s, &l);
            for i in (1..n).rev() {
                items.swap(i, rng.below(i + 1));
            }
            let (s, l) = split(&items);
            let after = real(&s, &l);
            assert!(
                same(before.0, after.0) && same(before.1, after.1),
                "{items:?}"
            );
            assert_eq!(before.2.tied_share.to_bits(), after.2.tied_share.to_bits());
        }
    }

    /// An earlier implementation, written apart from this one: ranked by
    /// `total_cmp` on the raw scores, so it splits -0.0 from +0.0 and puts
    /// a positive NaN on top; AP is not NaN without negatives.
    mod earlier {
        pub fn auroc(scores: &[f64], positive: &[bool]) -> f64 {
            let n_pos = positive.iter().filter(|&&p| p).count();
            let n_neg = positive.len() - n_pos;
            if n_pos == 0 || n_neg == 0 {
                return f64::NAN;
            }
            let mut order: Vec<usize> = (0..scores.len()).collect();
            order.sort_unstable_by(|&a, &b| scores[a].total_cmp(&scores[b]));
            let mut rank_sum = 0.0;
            let mut start = 0;
            while start < order.len() {
                let end = tie_end(&order, scores, start);
                let mid = (start + 1 + end) as f64 / 2.0;
                let pos = order[start..end].iter().filter(|&&i| positive[i]).count();
                rank_sum += mid * pos as f64;
                start = end;
            }
            let n_pos = n_pos as f64;
            (rank_sum - n_pos * (n_pos + 1.0) / 2.0) / (n_pos * n_neg as f64)
        }

        pub fn average_precision(scores: &[f64], positive: &[bool]) -> f64 {
            let n_pos = positive.iter().filter(|&&p| p).count();
            if n_pos == 0 {
                return f64::NAN;
            }
            let mut order: Vec<usize> = (0..scores.len()).collect();
            order.sort_unstable_by(|&a, &b| scores[b].total_cmp(&scores[a]));
            let (mut tp, mut ap) = (0usize, 0.0);
            let mut start = 0;
            while start < order.len() {
                let end = tie_end(&order, scores, start);
                let hits = order[start..end].iter().filter(|&&i| positive[i]).count();
                tp += hits;
                ap += hits as f64 / n_pos as f64 * tp as f64 / end as f64;
                start = end;
            }
            ap
        }

        fn tie_end(order: &[usize], scores: &[f64], start: usize) -> usize {
            let s = scores[order[start]];
            start
                + order[start..]
                    .iter()
                    .take_while(|&&i| scores[i].total_cmp(&s).is_eq())
                    .count()
        }
    }

    #[test]
    fn finite_scores_match_the_earlier_implementation() {
        let mut rng = Lcg(11);
        let alphabet = [-1.5, 0.25, 0.5, 0.5000000000000001, 2.0, f64::INFINITY];
        for _ in 0..500 {
            let n = 2 + rng.below(60);
            let values: Vec<f64> = (0..n)
                .map(|_| alphabet[rng.below(alphabet.len())])
                .collect();
            let mut labels: Vec<bool> = (0..n).map(|_| rng.below(4) == 0).collect();
            (labels[0], labels[1]) = (true, false);
            let scores: Vec<_> = values.iter().map(|&x| Some(x)).collect();
            let (roc, ap) = (auroc(&scores, &labels), average_precision(&scores, &labels));
            assert!(near(roc, earlier::auroc(&values, &labels)), "{values:?}");
            assert!(
                near(ap, earlier::average_precision(&values, &labels)),
                "{values:?}"
            );
        }
        // Where they part, by design: -0.0 against +0.0, and NaN.
        let (zeros, labels) = ([0.0, -0.0], [false, true]);
        assert_eq!(earlier::auroc(&zeros, &labels), 0.0);
        assert_eq!(auroc(&[Some(0.0), Some(-0.0)], &labels), 0.5);
        let (nan, labels) = ([f64::NAN, 0.1], [false, true]);
        assert_eq!(earlier::auroc(&nan, &labels), 0.0);
        assert_eq!(auroc(&[Some(f64::NAN), Some(0.1)], &labels), 1.0);
    }
}
