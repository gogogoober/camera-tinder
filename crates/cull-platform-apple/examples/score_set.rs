//! Score triage against the answer key of the test set: which photos got swept, which should have been,
//! and whether bursts grouped (and unrelated shots seconds apart did not).
//!
//!     cargo run --release -p cull-platform-apple --example score_set -- test-photos-v2/set [--rust-only] [-v]
//!
//! Reads `<dir>/manifest.json` (written by `make_test_set`) and the JPEGs under `<dir>/DCIM`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use chrono::NaiveDateTime;
use cull_core::analysis::{AnalysisImage, Analyzers, PhotoAnalysis};
use cull_core::model::{MediaKind, Photo};
use cull_core::preview;
use cull_core::triage::{self, Question};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(args.first().expect("usage: score_set <dir> [--rust-only] [-v]"));
    let verbose = args.iter().any(|a| a == "-v");
    let analyzers = if args.iter().any(|a| a == "--rust-only") { Analyzers::rust() } else { cull_platform_apple::vision::analyzers() };
    let manifest: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();

    let mut photos = Vec::new();
    let mut analyses: HashMap<i64, PhotoAnalysis> = HashMap::new();
    for (i, m) in manifest.iter().enumerate() {
        let id = i as i64 + 1;
        let file = m["file"].as_str().unwrap();
        let path = dir.join("DCIM/100TEST").join(file);
        let img = preview::decode(&std::fs::read(&path).unwrap()).unwrap();
        let img = AnalysisImage::from_jpeg(AnalysisImage::from_image(img).jpeg().unwrap().to_vec());
        let mut all = PhotoAnalysis::new(id);
        for kind in analyzers.available() {
            let r = analyzers.run(id, &img, &[kind], Some(&all));
            all.merge(r);
        }
        analyses.insert(id, all);
        photos.push(Photo {
            id,
            fingerprint: format!("fp{id}"),
            source_id: "set".into(),
            rel_path: file.into(),
            file_name: file.into(),
            kind: MediaKind::Jpeg,
            size: 1,
            captured_at: Some(NaiveDateTime::parse_from_str(m["time"].as_str().unwrap(), "%Y-%m-%dT%H:%M:%S").unwrap()),
            camera: None,
            decision: None,
        });
    }
    photos.sort_by_key(|p| p.captured_at);
    let items = triage::build(&photos, &analyses);

    let (mut swept_ok, mut swept_total_expected, mut wrongly_swept) = (0, 0, Vec::new());
    let mut missed = Vec::new();
    // group id (manifest) -> set of triage group ids; triage group id -> manifest groups
    let mut expected_groups: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    let mut by_triage: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    let mut wrong_groups = Vec::new();
    let manifest_of = |p: &Photo| &manifest[p.id as usize - 1];

    for (p, item) in photos.iter().zip(&items) {
        let m = manifest_of(p);
        let expect = m["expect"].as_str().unwrap();
        let name = p.file_name.clone();
        let note = m["note"].as_str().unwrap_or("");
        let swept = item.question == Question::Sweep;
        let reasons: Vec<String> = item.reasons.iter().map(|r| format!("{r:?}")).collect();
        if expect == "sweep" {
            swept_total_expected += 1;
            if swept { swept_ok += 1 } else { missed.push(format!("{name}  {note}  [hints {:?}]", item.hints)) }
        } else if swept {
            wrongly_swept.push(format!("{name}  expected {expect}  swept for {reasons:?}  ({note})"));
        }
        if let Some(g) = m["group"].as_str() {
            expected_groups.entry(g.into()).or_default().push(p.id);
            match &item.group {
                Some(tg) => by_triage.entry(tg.id).or_default().push(g.into()),
                None => {}
            }
        } else if let Some(tg) = &item.group {
            wrong_groups.push(format!("{name}  expected a single photo but is in a {:?} group of {}  ({note})", tg.kind, tg.size));
        }
        if verbose {
            println!("{name} {:<5} {:<6} {:?} reasons={reasons:?} hints={:?}  {note}", expect, item.question.as_str(), item.group.as_ref().map(|g| (g.kind, g.size)), item.hints);
        }
    }

    println!("\n== Sweep ==");
    println!("expected sweeps caught: {swept_ok}/{swept_total_expected}");
    for m in &missed { println!("  MISSED  {m}"); }
    println!("wrongly swept (should be kept for judging): {}", wrongly_swept.len());
    for w in &wrongly_swept { println!("  WRONG   {w}"); }
    let total_swept = items.iter().filter(|i| i.question == Question::Sweep).count();
    println!("total swept: {total_swept} of {}", items.len());

    println!("\n== Groups ==");
    let mut group_ok = 0;
    for (g, ids) in &expected_groups {
        let triage_ids: Vec<Option<i64>> = ids.iter().map(|id| items[photos.iter().position(|p| p.id == *id).unwrap()].group.as_ref().map(|t| t.id)).collect();
        let all_same = triage_ids.iter().all(|t| t.is_some() && *t == triage_ids[0]);
        let size = triage_ids.first().and_then(|t| *t).map(|t| by_triage.get(&t).map_or(0, |v| v.len())).unwrap_or(0);
        if all_same { group_ok += 1 }
        println!("  {:<12} {}  ({} frames expected, in one group of {size})", g, if all_same { "ok     " } else { "SPLIT/MISSING" }, ids.len());
    }
    println!("expected groups intact: {group_ok}/{}", expected_groups.len());
    println!("photos wrongly grouped: {}", wrong_groups.len());
    for w in &wrong_groups { println!("  WRONG   {w}"); }
}
