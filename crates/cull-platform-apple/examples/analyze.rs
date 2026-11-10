//! Run every photo analyzer on a folder of JPEGs and print what each found.
//!
//!     cargo run -p cull-platform-apple --example analyze -- <dir> [--rust-only] [--limit N] [--calibrate] [--json]
//!
//! - default: Rust + Apple Vision analyzers (what the macOS app uses)
//! - `--rust-only`: only the pure-Rust analyzers (what other platforms get)
//! - `--calibrate`: print similarity distances for near-duplicates (a crop/brightened copy)
//!   versus unrelated photos, per embedding model, to set `near_duplicate_distance`
//! - `--json`: print each photo's full `PhotoAnalysis` as JSON lines
//! - `--pairs`: print the 15 most similar pairs (embedding + hash distance), for tuning grouping

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::NaiveDateTime;
use cull_core::analysis::grouping::{Candidate, similar_groups};
use cull_core::analysis::issues::issues;
use cull_core::analysis::similarity::{ColorLayout, near_duplicate_distance};
use cull_core::analysis::{AnalysisImage, AnalysisKind, Analyzers, GroupingOptions, PhotoAnalysis, SimilarityEmbedder};
use cull_core::preview;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(args.first().expect("usage: analyze <dir> [--rust-only] [--limit N] [--calibrate] [--json]"));
    let rust_only = args.iter().any(|a| a == "--rust-only");
    let json = args.iter().any(|a| a == "--json");
    let limit: usize = args.iter().position(|a| a == "--limit").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);

    let mut files: Vec<PathBuf> = walk(&dir).into_iter().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"))).collect();
    files.sort();
    files.truncate(limit);
    if files.is_empty() {
        eprintln!("no JPEGs under {}", dir.display());
        return;
    }

    let analyzers = if rust_only { Analyzers::rust() } else { cull_platform_apple::vision::analyzers() };
    println!("Analyzers:");
    for info in analyzers.info() {
        println!("  {:<15} {}", info.kind.as_str(), info.implementation.as_deref().unwrap_or("— (not available)"));
    }

    if args.iter().any(|a| a == "--calibrate") {
        calibrate(&files, &analyzers);
        return;
    }

    let mut timings: BTreeMap<AnalysisKind, Duration> = BTreeMap::new();
    let mut results: Vec<(PathBuf, Option<NaiveDateTime>, PhotoAnalysis)> = Vec::new();
    let mut issue_counts: BTreeMap<String, usize> = BTreeMap::new();
    for (i, path) in files.iter().enumerate() {
        let bytes = std::fs::read(path).unwrap();
        let img = match preview::decode(&bytes) {
            // Same input the app uses: the 2048 px master preview as JPEG bytes.
            Ok(img) => AnalysisImage::from_jpeg(AnalysisImage::from_image(img).jpeg().unwrap().to_vec()),
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                continue;
            }
        };
        let mut all = PhotoAnalysis::new(i as i64);
        for kind in analyzers.available() {
            let t = Instant::now();
            let r = analyzers.run(i as i64, &img, &[kind], Some(&all));
            *timings.entry(kind).or_default() += t.elapsed();
            all.merge(r);
        }
        let found = issues(&all);
        for issue in &found {
            *issue_counts.entry(format!("{:?}", issue.kind)).or_default() += 1;
        }
        if json {
            println!("{}", serde_json::to_string(&all.clone().without_embedding()).unwrap());
        } else {
            let faces = all.faces.as_ref().map(|f| f.len()).unwrap_or(0);
            let eyes: Vec<String> = all.eyes.iter().flatten().map(|e| format!("{:.2}/{:.2}", e.left_openness.unwrap_or(-1.0), e.right_openness.unwrap_or(-1.0))).collect();
            println!(
                "{:<40} blur={:>3}/{:<3} faces={} eyes=[{}] aest={} issues=[{}]{}",
                path.file_name().unwrap().to_string_lossy(),
                all.blur.as_ref().map(|b| b.score).unwrap_or(0),
                all.blur.as_ref().map(|b| b.peak).unwrap_or(0),
                faces,
                eyes.join(" "),
                all.aesthetics.as_ref().map(|a| format!("{:+.2}{}", a.overall, if a.is_utility { "U" } else { "" })).unwrap_or("-".into()),
                found.iter().map(|i| format!("{:?}", i.kind)).collect::<Vec<_>>().join(","),
                if all.errors.is_empty() { String::new() } else { format!(" ERRORS {:?}", all.errors) },
            );
        }
        let captured = cull_core::metadata::read_meta(&mut std::io::Cursor::new(&bytes)).captured_at.or_else(|| {
            std::fs::metadata(path).ok()?.modified().ok().map(|t| chrono::DateTime::<chrono::Local>::from(t).naive_local())
        });
        results.push((path.clone(), captured, all));
    }

    println!("\nAverage time per photo:");
    for (k, d) in &timings {
        println!("  {:<15} {:>7.1} ms", k.as_str(), d.as_secs_f64() * 1000.0 / results.len() as f64);
    }
    println!("  {:<15} {:>7.1} ms", "TOTAL", timings.values().sum::<Duration>().as_secs_f64() * 1000.0 / results.len() as f64);

    println!("\nIssues found:");
    for (k, n) in &issue_counts {
        println!("  {k:<20} {n}");
    }

    results.sort_by_key(|r| r.1);
    let candidates: Vec<Candidate> = results.iter().map(|(_, t, a)| Candidate { captured_at: *t, analysis: a }).collect();
    let groups = similar_groups(&candidates, &GroupingOptions { max_gap_secs: None, ..Default::default() });
    println!("\nNear-duplicate groups (no time limit): {}", groups.len());
    let name = |id: i64| results.iter().find(|r| r.2.photo_id == id).map(|r| r.0.file_name().unwrap().to_string_lossy().to_string()).unwrap_or_default();
    for g in &groups {
        println!("  best {} ← {}", name(g.best), g.photo_ids.iter().skip(1).map(|id| name(*id)).collect::<Vec<_>>().join(", "));
    }
    if args.iter().any(|a| a == "--pairs") {
        let mut pairs = Vec::new();
        for (i, a) in results.iter().enumerate() {
            for b in &results[i + 1..] {
                if let (Some(x), Some(y)) = (&a.2.similarity, &b.2.similarity)
                    && let Some(d) = x.distance(y)
                {
                    let h = a.2.perceptual_hash.zip(b.2.perceptual_hash).map(|(p, q)| p.distance(&q));
                    pairs.push((d, h, name(a.2.photo_id), name(b.2.photo_id)));
                }
            }
        }
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        println!("\nClosest pairs (embedding distance, hash bits):");
        for (d, h, a, b) in pairs.iter().take(15) {
            println!("  {d:.3}  {:>2}  {a} ~ {b}", h.map(|h| h.to_string()).unwrap_or("-".into()));
        }
    }
}

fn walk(dir: &PathBuf) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() { out.extend(walk(&p)) } else { out.push(p) }
    }
    out
}

/// Distances between each photo and a near-duplicate of itself (3% crop each side, +12
/// brightness) versus distances between unrelated photos.
fn calibrate(files: &[PathBuf], analyzers: &Analyzers) {
    let mut embedders: Vec<(&str, &dyn SimilarityEmbedder)> = vec![("color_layout", &ColorLayout)];
    if let Some(s) = &analyzers.similarity
        && s.implementation() != ColorLayout.implementation()
    {
        embedders.push((s.implementation(), s.as_ref()));
    }
    for (label, e) in embedders {
        let mut same = Vec::new();
        let mut originals = Vec::new();
        for path in files.iter().take(40) {
            let Ok(img) = preview::decode(&std::fs::read(path).unwrap()) else { continue };
            let (w, h) = (img.width(), img.height());
            let variant = image::DynamicImage::ImageRgb8(img.crop_imm(w * 3 / 100, h * 3 / 100, w * 94 / 100, h * 94 / 100).brighten(12).to_rgb8());
            let a = e.embed(&AnalysisImage::from_image(img)).unwrap();
            let b = e.embed(&AnalysisImage::from_image(variant)).unwrap();
            same.push(a.distance(&b).unwrap());
            originals.push(a);
        }
        let mut diff = Vec::new();
        for i in 0..originals.len() {
            for j in i + 1..originals.len() {
                diff.push(originals[i].distance(&originals[j]).unwrap());
            }
        }
        same.sort_by(f32::total_cmp);
        diff.sort_by(f32::total_cmp);
        let pct = |v: &[f32], q: f64| v[((v.len() - 1) as f64 * q) as usize];
        println!(
            "\n{label}: near-dup distance p50={:.3} p90={:.3} max={:.3} | unrelated min={:.3} p01={:.3} p10={:.3} p50={:.3} | threshold={:.3}",
            pct(&same, 0.5),
            pct(&same, 0.9),
            same.last().unwrap(),
            diff[0],
            pct(&diff, 0.01),
            pct(&diff, 0.1),
            pct(&diff, 0.5),
            near_duplicate_distance(&originals[0].model)
        );
    }
}
