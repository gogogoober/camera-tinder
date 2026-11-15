//! Fit the model on the user's decisions and measure it honestly.
//!
//! The measurement trains on the older decisions and tests on the newest ones. Frames of one burst are
//! kept together (they look alike, so splitting them would flatter the model). Two numbers are compared
//! on the same held-out decisions, for the proposed model, the model in use now, and the hand-tuned
//! `quality_score`:
//! - **AUC**: pick a random Keep and a random Nope; how often does the Keep score higher? (0.5 = coin flip)
//! - **Burst picks**: in a group where you kept exactly one frame, how often was that frame ranked first?

use std::collections::HashMap;

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use super::{FeatureWeight, WeightChange, Weights, features, is_people, names, weight_changes};
use crate::analysis::grouping::{Candidate, similar_groups};
use crate::analysis::ranking::quality_score;
use crate::analysis::{GroupingOptions, PhotoAnalysis};
use crate::model::PhotoId;
use crate::moments::BURST_GAP_SECS;

/// One decided photo with its current analysis.
pub struct Sample {
    pub photo_id: PhotoId,
    pub captured_at: Option<NaiveDateTime>,
    /// Keep or Favorite (true); Nope or Hate (false).
    pub keep: bool,
    pub favorite: bool,
    pub decided_at: String,
    pub analysis: PhotoAnalysis,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub l2: f32,
    pub learning_rate: f32,
    pub iterations: usize,
    /// Share of the newest decisions kept out of fitting, to test on.
    pub holdout_fraction: f32,
    /// Fewer Keeps or Nopes than this and no model is proposed.
    pub min_per_class: usize,
    /// How much more a Favorite counts than a Keep when fitting.
    pub favorite_weight: f32,
    /// How much the within-burst choices count next to the whole library (1.0 = as much). 0 by default:
    /// cross-validation on real decisions showed they hurt burst picks (see `cross_validate`); revisit with more data.
    pub pair_share: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self { l2: 0.01, learning_rate: 0.1, iterations: 5000, holdout_fraction: 0.2, min_per_class: 100, favorite_weight: 2.0, pair_share: 0.0 }
    }
}

/// How one ranking did on the held-out decisions.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Eval {
    /// `None` when the held-out decisions hold only one kind.
    pub auc: Option<f32>,
    pub burst_hits: u32,
    pub burst_groups: u32,
    /// Hits expected from picking a frame at random in those groups.
    #[serde(default)]
    pub burst_chance: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    pub decisions: u32,
    pub keeps: u32,
    pub nopes: u32,
    pub favorites: u32,
    pub trained_on: u32,
    pub held_out: u32,
    /// Groups (bursts) with a Keep and a Nope that taught the within-burst choices.
    #[serde(default)]
    pub pair_groups: u32,
    /// Decisions on photos with people (faces), and how many of those were Keeps.
    #[serde(default)]
    pub people_decisions: u32,
    #[serde(default)]
    pub people_keeps: u32,
    /// The proposed model, fitted without the held-out decisions.
    pub candidate: Eval,
    /// The model in use now, or the hand-tuned score when none is approved.
    pub current: Eval,
    /// The hand-tuned `quality_score`, always.
    pub hand_tuned: Eval,
}

/// A feature's difference between Keeps and Nopes, in standard deviations (Cohen's d; + = Keeps have more).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Separation {
    pub name: String,
    pub effect: f32,
    pub keep_mean: f32,
    pub nope_mean: f32,
    pub with_value: u32,
}

#[derive(Debug, Clone)]
pub struct Proposal {
    /// Fitted on every decision.
    pub weights: Weights,
    pub metrics: Metrics,
    pub changes: Vec<WeightChange>,
    pub separation: Vec<Separation>,
}

/// Fit on `rows` of (standardized-later raw features, label, weight).
type Row = (Vec<Option<f32>>, f32, f32);

/// A Keep and a Nope from the same group: the Keep should score higher. Weight sums to 1 per group.
type Pair = (Vec<Option<f32>>, Vec<Option<f32>>, f32);

fn fit(rows: &[Row], pairs: &[Pair], cfg: &Config) -> Weights {
    let n = names().len();
    let (mut mean, mut std) = (vec![0.0f32; n], vec![1.0f32; n]);
    for j in 0..n {
        let present: Vec<f32> = rows.iter().filter_map(|r| r.0[j]).collect();
        if present.len() >= 2 {
            let m = present.iter().sum::<f32>() / present.len() as f32;
            let var = present.iter().map(|x| (x - m).powi(2)).sum::<f32>() / present.len() as f32;
            mean[j] = m;
            std[j] = if var.sqrt() > 1e-6 { var.sqrt() } else { 1.0 };
        }
    }
    let x: Vec<Vec<f32>> = rows.iter().map(|r| (0..n).map(|j| r.0[j].map_or(0.0, |v| (v - mean[j]) / std[j])).collect()).collect();
    // Where both frames have a value, the difference in standard deviations; otherwise no information (0).
    let diffs: Vec<Vec<f32>> = pairs.iter().map(|p| (0..n).map(|j| if let (Some(a), Some(c)) = (p.0[j], p.1[j]) { (a - c) / std[j] } else { 0.0 }).collect()).collect();
    let rows_total: f32 = rows.iter().map(|r| r.2).sum();
    let pairs_raw: f32 = pairs.iter().map(|p| p.2).sum();
    // The pairs together weigh `pair_share` times the rows, however many bursts there are.
    let pair_scale = if pairs_raw > 0.0 { cfg.pair_share * rows_total / pairs_raw } else { 0.0 };
    let total = rows_total * (1.0 + if pairs_raw > 0.0 { cfg.pair_share } else { 0.0 });
    let (mut w, mut b) = (vec![0.0f32; n], 0.0f32);
    for _ in 0..cfg.iterations {
        let (mut gw, mut gb) = (vec![0.0f32; n], 0.0f32);
        for (xi, r) in x.iter().zip(rows) {
            let z = b + w.iter().zip(xi).map(|(a, c)| a * c).sum::<f32>();
            let err = r.2 * (1.0 / (1.0 + (-z).exp()) - r.1);
            gb += err;
            for j in 0..n {
                gw[j] += err * xi[j];
            }
        }
        for (d, p) in diffs.iter().zip(pairs) {
            let z: f32 = w.iter().zip(d).map(|(a, c)| a * c).sum();
            let err = p.2 * pair_scale * (1.0 / (1.0 + (-z).exp()) - 1.0);
            for j in 0..n {
                gw[j] += err * d[j];
            }
        }
        b -= cfg.learning_rate * gb / total;
        for j in 0..n {
            w[j] -= cfg.learning_rate * (gw[j] / total + cfg.l2 * w[j]);
        }
    }
    Weights { bias: b, features: (0..n).map(|j| FeatureWeight { name: names()[j].clone(), weight: w[j], mean: mean[j], std: std[j] }).collect() }
}

/// Rows for fitting: Nopes and Keeps weigh the same in total, a Favorite counts extra.
fn rows_of(samples: &[&Sample], cfg: &Config) -> Vec<Row> {
    let keeps = samples.iter().filter(|s| s.keep).count().max(1) as f32;
    let nopes = samples.iter().filter(|s| !s.keep).count().max(1) as f32;
    let total = samples.len() as f32;
    samples
        .iter()
        .map(|s| {
            let class = if s.keep { total / (2.0 * keeps) } else { total / (2.0 * nopes) };
            (features(&s.analysis), if s.keep { 1.0 } else { 0.0 }, class * if s.favorite { cfg.favorite_weight } else { 1.0 })
        })
        .collect()
}

/// Every (Keep, Nope) pair inside a group, among `groups` made only of photos in `allowed`.
fn pairs_of(samples: &[Sample], groups: &[Vec<usize>], allowed: &std::collections::HashSet<usize>) -> (Vec<Pair>, u32) {
    let (mut out, mut used) = (Vec::new(), 0);
    for g in groups.iter().filter(|g| g.iter().all(|i| allowed.contains(i))) {
        let keeps: Vec<usize> = g.iter().copied().filter(|&i| samples[i].keep).collect();
        let nopes: Vec<usize> = g.iter().copied().filter(|&i| !samples[i].keep).collect();
        if keeps.is_empty() || nopes.is_empty() {
            continue;
        }
        used += 1;
        let weight = 1.0 / (keeps.len() * nopes.len()) as f32;
        for &k in &keeps {
            for &n in &nopes {
                out.push((features(&samples[k].analysis), features(&samples[n].analysis), weight));
            }
        }
    }
    (out, used)
}

/// Probability that a random Keep outranks a random Nope (ties count half).
fn auc(scored: &[(f32, bool)]) -> Option<f32> {
    let pos = scored.iter().filter(|s| s.1).count();
    let neg = scored.len() - pos;
    if pos == 0 || neg == 0 {
        return None;
    }
    let mut order: Vec<&(f32, bool)> = scored.iter().collect();
    order.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (mut rank_sum, mut i) = (0.0f64, 0);
    while i < order.len() {
        let mut j = i;
        while j + 1 < order.len() && order[j + 1].0 == order[i].0 {
            j += 1;
        }
        let avg_rank = (i + j) as f64 / 2.0 + 1.0;
        rank_sum += avg_rank * order[i..=j].iter().filter(|s| s.1).count() as f64;
        i = j + 1;
    }
    Some(((rank_sum - pos as f64 * (pos as f64 + 1.0) / 2.0) / (pos as f64 * neg as f64)) as f32)
}

/// The burst / near-duplicate groups among the decided photos, as indexes into `samples`.
fn groups_of(samples: &[Sample]) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..samples.len()).collect();
    order.sort_by_key(|&i| (samples[i].captured_at, samples[i].photo_id));
    let candidates: Vec<Candidate> = order.iter().map(|&i| Candidate { captured_at: samples[i].captured_at, analysis: &samples[i].analysis }).collect();
    let index: HashMap<PhotoId, usize> = samples.iter().enumerate().map(|(i, s)| (s.photo_id, i)).collect();
    let options = GroupingOptions { burst_gap_secs: Some(BURST_GAP_SECS), ..Default::default() };
    similar_groups(&candidates, &options).into_iter().map(|g| g.photo_ids.iter().map(|id| index[id]).collect()).collect()
}

fn evaluate(samples: &[Sample], held_out: &[usize], groups: &[Vec<usize>], score: &dyn Fn(&PhotoAnalysis) -> f32) -> Eval {
    let scored: Vec<(f32, bool)> = held_out.iter().map(|&i| (score(&samples[i].analysis), samples[i].keep)).collect();
    let in_hold: std::collections::HashSet<usize> = held_out.iter().copied().collect();
    let (mut hits, mut counted, mut chance) = (0, 0, 0.0f32);
    for g in groups.iter().filter(|g| g.iter().all(|i| in_hold.contains(i))) {
        let kept: Vec<usize> = g.iter().copied().filter(|&i| samples[i].keep).collect();
        if kept.len() != 1 || g.len() < 2 {
            continue;
        }
        counted += 1;
        chance += 1.0 / g.len() as f32;
        let best = g.iter().copied().max_by(|&a, &b| score(&samples[a].analysis).total_cmp(&score(&samples[b].analysis)).then(b.cmp(&a))).unwrap();
        hits += (best == kept[0]) as u32;
    }
    Eval { auc: auc(&scored), burst_hits: hits, burst_groups: counted, burst_chance: chance }
}

/// Which feature values differ most between Keeps and Nopes.
fn separation(samples: &[Sample]) -> Vec<Separation> {
    let mut out = Vec::new();
    for (j, name) in names().iter().enumerate() {
        let side = |keep: bool| -> Vec<f32> { samples.iter().filter(|s| s.keep == keep).filter_map(|s| features(&s.analysis)[j]).collect() };
        let (k, n) = (side(true), side(false));
        if k.len() < 30 || n.len() < 30 {
            continue;
        }
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
        let var = |v: &[f32], m: f32| v.iter().map(|x| (x - m).powi(2)).sum::<f32>() / (v.len() - 1) as f32;
        let (mk, mn) = (mean(&k), mean(&n));
        let pooled = (((k.len() - 1) as f32 * var(&k, mk) + (n.len() - 1) as f32 * var(&n, mn)) / (k.len() + n.len() - 2) as f32).sqrt();
        if pooled > 1e-9 {
            out.push(Separation { name: name.to_string(), effect: (mk - mn) / pooled, keep_mean: mk, nope_mean: mn, with_value: (k.len() + n.len()) as u32 });
        }
    }
    out.sort_by(|a, b| b.effect.abs().total_cmp(&a.effect.abs()));
    out
}

/// Decisions grouped into units that must stay together (a whole burst, or one photo), oldest first.
fn units_by_time(samples: &[Sample], groups: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let mut unit_of: Vec<usize> = (0..samples.len()).collect();
    for (u, g) in groups.iter().enumerate() {
        for &i in g {
            unit_of[i] = samples.len() + u;
        }
    }
    let mut units: HashMap<usize, (String, Vec<usize>)> = HashMap::new();
    for (i, s) in samples.iter().enumerate() {
        let e = units.entry(unit_of[i]).or_insert_with(|| (s.decided_at.clone(), Vec::new()));
        e.0 = e.0.clone().min(s.decided_at.clone());
        e.1.push(i);
    }
    let mut ordered: Vec<(String, Vec<usize>)> = units.into_values().collect();
    ordered.sort();
    ordered.into_iter().map(|u| u.1).collect()
}

/// One way of ranking, scored over every fold of a cross-validation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CvRow {
    pub label: String,
    /// Mean AUC over the folds.
    pub auc: Option<f32>,
    pub burst_hits: u32,
    pub burst_groups: u32,
    pub burst_chance: f32,
}

/// Test several ways of training on *all* your decisions, so burst picks are counted over every burst you
/// judged instead of one small slice. Decisions go into `folds` parts (a burst is never split); each part is
/// ranked by models fitted on the other parts. Every variant sees the same parts.
pub fn cross_validate(samples: &[Sample], cfg: &Config, folds: usize) -> Result<Vec<CvRow>, String> {
    let groups = groups_of(samples);
    let units = units_by_time(samples, &groups);
    if folds < 2 || units.len() < folds * 10 {
        return Err("too few decisions to cross-validate".into());
    }
    let fold_of: Vec<usize> = (0..units.len()).map(|u| u % folds).collect(); // interleaved: every fold spans the whole history
    let part = |f: usize, held: bool| -> Vec<usize> { units.iter().zip(&fold_of).filter(|(_, k)| (**k == f) == held).flat_map(|(u, _)| u.iter().copied()).collect() };

    // (label, pair_share); `None` = the hand-tuned score, not fitted.
    let variants: [(&str, Option<f32>); 5] = [
        ("hand-tuned score", None),
        ("model, whole library only", Some(0.0)),
        ("model, + burst choices (x1)", Some(1.0)),
        ("model, + burst choices (x3)", Some(3.0)),
        ("model, mostly burst choices (x10)", Some(10.0)),
    ];
    let mut out = Vec::new();
    for (label, share) in variants {
        let (mut aucs, mut hits, mut counted, mut chance) = (Vec::new(), 0, 0, 0.0f32);
        for f in 0..folds {
            let (train_idx, hold_idx) = (part(f, false), part(f, true));
            let train_refs: Vec<&Sample> = train_idx.iter().map(|&i| &samples[i]).collect();
            let e = match share {
                None => evaluate(samples, &hold_idx, &groups, &quality_score),
                Some(share) => {
                    let train_set: std::collections::HashSet<usize> = train_idx.iter().copied().collect();
                    let (pairs, _) = pairs_of(samples, &groups, &train_set);
                    let w = fit(&rows_of(&train_refs, cfg), &pairs, &Config { pair_share: share, ..cfg.clone() });
                    evaluate(samples, &hold_idx, &groups, &|a| w.score(a))
                }
            };
            aucs.extend(e.auc);
            hits += e.burst_hits;
            counted += e.burst_groups;
            chance += e.burst_chance;
        }
        let auc = if aucs.is_empty() { None } else { Some(aucs.iter().sum::<f32>() / aucs.len() as f32) };
        out.push(CvRow { label: label.to_string(), auc, burst_hits: hits, burst_groups: counted, burst_chance: chance });
    }
    Ok(out)
}

pub fn cv_report(rows: &[CvRow], folds: usize) -> String {
    let mut s = format!("Cross-validated over all your decisions ({folds} parts, bursts kept whole; each part tested by models fitted on the rest):\n");
    for r in rows {
        let auc = r.auc.map_or("n/a".to_string(), |a| format!("{a:.3}"));
        s += &format!("  {:<36} AUC {auc:<6}  burst picks {} (random pick: {:.0}%)\n", r.label, pct(r.burst_hits, r.burst_groups), if r.burst_groups == 0 { 0.0 } else { 100.0 * r.burst_chance / r.burst_groups as f32 });
    }
    s
}

/// Fit a model, measure it on the newest decisions, and describe what it would change.
/// `current` is the approved model in use (None = the hand-tuned ranking).
pub fn propose(samples: &[Sample], current: Option<&Weights>, cfg: &Config) -> Result<Proposal, String> {
    let keeps = samples.iter().filter(|s| s.keep).count();
    let nopes = samples.len() - keeps;
    if keeps < cfg.min_per_class || nopes < cfg.min_per_class {
        return Err(format!("not enough decisions yet: {keeps} Keeps and {nopes} Nopes (need {} of each)", cfg.min_per_class));
    }

    // Hold out the newest decisions, whole groups at a time.
    let groups = groups_of(samples);
    let ordered = units_by_time(samples, &groups);
    let cut = ((ordered.len() as f32) * (1.0 - cfg.holdout_fraction)).round() as usize;
    let (train_idx, hold_idx): (Vec<usize>, Vec<usize>) = {
        let (a, b) = ordered.split_at(cut.min(ordered.len()));
        (a.iter().flatten().copied().collect(), b.iter().flatten().copied().collect())
    };

    let train_refs: Vec<&Sample> = train_idx.iter().map(|&i| &samples[i]).collect();
    if !(train_refs.iter().any(|s| s.keep) && train_refs.iter().any(|s| !s.keep)) {
        return Err("the older decisions hold only one kind of decision".into());
    }
    let train_set: std::collections::HashSet<usize> = train_idx.iter().copied().collect();
    let (train_pairs, _) = if cfg.pair_share > 0.0 { pairs_of(samples, &groups, &train_set) } else { (Vec::new(), 0) };
    let tested = fit(&rows_of(&train_refs, cfg), &train_pairs, cfg);

    let candidate = evaluate(samples, &hold_idx, &groups, &|a| tested.score(a));
    let hand_tuned = evaluate(samples, &hold_idx, &groups, &quality_score);
    let in_use = match current {
        Some(w) => evaluate(samples, &hold_idx, &groups, &|a| w.score(a)),
        None => hand_tuned,
    };

    let all_refs: Vec<&Sample> = samples.iter().collect();
    let all_set: std::collections::HashSet<usize> = (0..samples.len()).collect();
    let (all_pairs, pair_groups) = if cfg.pair_share > 0.0 { pairs_of(samples, &groups, &all_set) } else { (Vec::new(), 0) };
    let weights = fit(&rows_of(&all_refs, cfg), &all_pairs, cfg);
    let metrics = Metrics {
        decisions: samples.len() as u32,
        keeps: keeps as u32,
        nopes: nopes as u32,
        favorites: samples.iter().filter(|s| s.favorite).count() as u32,
        trained_on: train_idx.len() as u32,
        held_out: hold_idx.len() as u32,
        pair_groups,
        people_decisions: samples.iter().filter(|s| is_people(&s.analysis) == Some(true)).count() as u32,
        people_keeps: samples.iter().filter(|s| s.keep && is_people(&s.analysis) == Some(true)).count() as u32,
        candidate,
        current: in_use,
        hand_tuned,
    };
    Ok(Proposal { changes: weight_changes(current, &weights), separation: separation(samples), weights, metrics })
}

fn pct(hits: u32, of: u32) -> String {
    if of == 0 { "n/a".into() } else { format!("{hits}/{of} ({:.0}%)", 100.0 * hits as f32 / of as f32) }
}

fn eval_line(label: &str, e: &Eval) -> String {
    let auc = e.auc.map_or("n/a".to_string(), |a| format!("{a:.3}"));
    format!("  {label:<34} AUC {auc:<6}  burst picks {}\n", pct(e.burst_hits, e.burst_groups))
}

impl Proposal {
    /// The text shown before the user approves: what the data says, how the model tested, what moves.
    pub fn report(&self, has_current: bool) -> String {
        let m = &self.metrics;
        let mut s = String::new();
        s += &format!(
            "Decisions used: {} ({} Keep incl. {} Favorite, {} Nope/Hate). Tested on the newest {} (whole bursts), fitted on the older {}.{}\n\n",
            m.decisions, m.keeps, m.favorites, m.nopes, m.held_out, m.trained_on,
            if m.pair_groups > 0 { format!(" {} bursts taught it your within-burst choices.", m.pair_groups) } else { String::new() }
        );
        let things = (m.decisions - m.people_decisions, m.keeps - m.people_keeps);
        let rate = |k: u32, of: u32| if of == 0 { 0.0 } else { 100.0 * k as f32 / of as f32 };
        s += &format!(
            "People photos: {} decisions, {:.0}% Keep.  Things: {} decisions, {:.0}% Keep.  (People get their own weights on top of the shared ones.)\n\n",
            m.people_decisions, rate(m.people_keeps, m.people_decisions), m.decisions - m.people_decisions, rate(things.1, m.decisions - m.people_decisions)
        );
        s += "How well each ranking predicts your newest decisions (higher is better; AUC 0.5 = guessing):\n";
        s += &eval_line("hand-tuned score", &m.hand_tuned);
        if has_current {
            s += &eval_line("model in use now", &m.current);
        }
        s += &eval_line("proposed model", &m.candidate);
        s += "\nWhat separates your Keeps from your Nopes (effect in standard deviations; + = Keeps have more):\n";
        for x in self.separation.iter().take(8) {
            s += &format!("  {:<20} {:+.2}   (Keep avg {:.2}, Nope avg {:.2})\n", x.name, x.effect, x.keep_mean, x.nope_mean);
        }
        s += "\nWeights that would move (per standard deviation of the feature; + = pushes toward Keep):\n";
        for c in self.changes.iter().filter(|c| c.delta.abs() >= 0.02).take(12) {
            let old = c.old.map_or("  none".to_string(), |o| format!("{o:+.2}"));
            s += &format!("  {:<20} {old} -> {:+.2}   ({:+.2})\n", c.name, c.new, c.delta);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Aesthetics, Blur, BlurVerdict};

    fn sample(id: PhotoId, keep: bool, aesthetics: f32, blur: u8) -> Sample {
        let mut a = PhotoAnalysis::new(id);
        a.aesthetics = Some(Aesthetics { overall: aesthetics, is_utility: false });
        a.blur = Some(Blur { score: blur, peak: blur, directionality: 0.1, verdict: BlurVerdict::Sharp, edge_sharpness: None });
        Sample { photo_id: id, captured_at: None, keep, favorite: false, decided_at: format!("2026-11-{:02}T10:00:{:02}", 1 + id / 60, id % 60), analysis: a }
    }

    /// Keeps have higher aesthetics, blur is noise: the fit finds that and tests well on the newest photos.
    #[test]
    fn learns_the_feature_that_matters_and_tests_well() {
        let samples: Vec<Sample> = (0..400)
            .map(|i| {
                let keep = i % 3 == 0;
                let noise = ((i * 7919) % 100) as f32 / 100.0;
                sample(i, keep, if keep { 0.4 + 0.5 * noise } else { -0.4 + 0.5 * noise }, ((i * 31) % 100) as u8)
            })
            .collect();
        let p = propose(&samples, None, &Config::default()).unwrap();
        assert!(p.weights.weight_of("aesthetics").unwrap() > 1.0);
        assert!(p.weights.weight_of("aesthetics").unwrap() > 5.0 * p.weights.weight_of("blurScore").unwrap().abs());
        assert!(p.metrics.candidate.auc.unwrap() > 0.9);
        assert_eq!(p.separation[0].name, "aesthetics");
        assert_eq!(p.changes[0].name, "aesthetics");
    }

    /// The signal exists only inside bursts (the sharper frame is the Keep; overall sharpness is unrelated):
    /// pair training finds it, and the model then picks the kept frame of held-out bursts.
    #[test]
    fn learns_which_frame_of_a_burst_you_keep() {
        use crate::analysis::PerceptualHash;
        let mut samples = Vec::new();
        for g in 0..150i64 {
            let base = NaiveDateTime::parse_from_str("2026-11-01T08:00:00", "%Y-%m-%dT%H:%M:%S").unwrap() + chrono::Duration::hours(g);
            let hash = (g as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let level = ((g * 37) % 60) as u8; // how sharp this scene is overall: unrelated to the choice
            for f in 0..3i64 {
                let id = g * 3 + f;
                let mut s = sample(id, f == 1, 0.0, level + (f as u8) * 10);
                s.analysis.perceptual_hash = Some(PerceptualHash(hash));
                s.captured_at = Some(base + chrono::Duration::seconds(f));
                s.decided_at = format!("2026-11-02T{:02}:{:02}:{:02}", g / 60, g % 60, f);
                // The kept frame is the sharpest of its burst.
                if f == 1 {
                    s.analysis.blur.as_mut().unwrap().peak = level + 40;
                }
                samples.push(s);
            }
        }
        let p = propose(&samples, None, &Config { pair_share: 1.0, ..Config::default() }).unwrap();
        assert_eq!(p.metrics.pair_groups, 150);
        assert!(p.weights.weight_of("blurPeak").unwrap() > 0.0);
        let m = p.metrics.candidate;
        assert!(m.burst_groups >= 20 && m.burst_hits as f32 >= 0.9 * m.burst_groups as f32, "picked {}/{}", m.burst_hits, m.burst_groups);
    }

    /// Sharpness decides Keeps in people photos, aesthetics in photos of things: the people-only weights carry that
    /// difference, and the shared ones stay small for the feature that only matters for one kind.
    #[test]
    fn people_and_things_get_their_own_weights() {
        use crate::analysis::Face;
        use crate::analysis::geometry::NormRect;
        let face = Face { bounds: NormRect::new(0.4, 0.4, 0.1, 0.1), confidence: 0.9, roll: None, yaw: None, pitch: None, left_eye: vec![], right_eye: vec![] };
        let samples: Vec<Sample> = (0..800)
            .map(|i| {
                let people = i % 2 == 0;
                let keep = (i / 2) % 3 == 0;
                let noise = ((i * 7919) % 100) as f32 / 100.0;
                let (aesthetics, blur) = if people { (noise - 0.5, if keep { 70 } else { 20 } + (i % 10) as u8) } else { (if keep { 0.4 } else { -0.4 } + 0.3 * noise, ((i * 31) % 100) as u8) };
                let mut s = sample(i, keep, aesthetics, blur);
                s.analysis.faces = Some(if people { vec![face.clone()] } else { vec![] });
                s
            })
            .collect();
        let p = propose(&samples, None, &Config::default()).unwrap();
        assert!(p.metrics.people_decisions == 400);
        let w = |n: &str| p.weights.weight_of(n).unwrap();
        assert!(w("people·blurPeak") + w("people·blurScore") > 1.0, "people: sharpness matters");
        assert!(w("aesthetics") > 1.0, "things: aesthetics matters");
        assert!(w("people·aesthetics") < -0.5, "...and the people adjustment cancels it for people");
        assert!(p.metrics.candidate.auc.unwrap() > 0.85);
    }

    #[test]
    fn refuses_to_propose_with_too_few_decisions() {
        let samples: Vec<Sample> = (0..40).map(|i| sample(i, i % 2 == 0, 0.0, 50)).collect();
        assert!(propose(&samples, None, &Config::default()).unwrap_err().contains("not enough"));
    }

    #[test]
    fn auc_handles_ties_and_one_sided_data() {
        assert_eq!(auc(&[(1.0, true), (0.0, false)]), Some(1.0));
        assert_eq!(auc(&[(1.0, true), (1.0, false)]), Some(0.5));
        assert_eq!(auc(&[(1.0, true)]), None);
    }
}
