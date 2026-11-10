//! Photo analysis through `CullerService`: scan → analyze → stored, incremental, issues, groups.

use std::sync::Arc;

use cull_core::analysis::{AnalysisKind, AnalyzeRequest, GroupingOptions, IssueKind};
use cull_core::*;
use cull_platform_fs::FsSource;
use image::{ImageBuffer, Rgb};

fn write(path: &std::path::Path, f: impl Fn(u32, u32) -> [u8; 3]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    ImageBuffer::from_fn(900, 600, |x, y| Rgb(f(x, y))).save(path).unwrap();
}

fn scene(x: u32, y: u32, shift: u32) -> [u8; 3] {
    let v = (((x + shift) as f32 / 23.0).sin() * 70.0 + (y as f32 / 17.0).cos() * 50.0 + 128.0) as u8;
    [v, v / 2 + 40, 255 - v]
}

#[test]
fn analyze_store_issues_groups() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    // Two near-identical shots, one unrelated, one black frame.
    write(&card.join("a.jpg"), |x, y| scene(x, y, 0));
    write(&card.join("b.jpg"), |x, y| scene(x, y, 2));
    write(&card.join("c.jpg"), |x, y| if (x / 40 + y / 40) % 2 == 0 { [230, 230, 230] } else { [20, 90, 30] });
    write(&card.join("d.jpg"), |_, _| [2, 2, 3]);

    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    // Analysis must work from the preview cache alone: "unplug the card".
    std::fs::rename(&card, tmp.path().join("unplugged")).unwrap();

    let info = svc.analyzer_info();
    assert_eq!(info.len(), AnalysisKind::ALL.len());
    assert!(info.iter().find(|i| i.kind == AnalysisKind::Faces).unwrap().implementation.is_none(), "no face engine by default");

    let r = svc.analyze(&AnalyzeRequest::default(), &NullSink).unwrap();
    assert_eq!((r.photos, r.analyzed, r.up_to_date, r.kind_errors), (4, 4, 0, 0), "{r:?}");
    assert!(r.failed.is_empty());

    // Second run: everything is current.
    let again = svc.analyze(&AnalyzeRequest::default(), &NullSink).unwrap();
    assert_eq!((again.analyzed, again.up_to_date), (0, 4));
    // Force one kind on one photo.
    let all = svc.queue(&Filter { status: StatusFilter::All, ..Default::default() }).unwrap();
    let forced = svc
        .analyze(&AnalyzeRequest { photo_ids: Some(vec![all[0].id]), kinds: vec![AnalysisKind::Blur], force: true, ..Default::default() }, &NullSink)
        .unwrap();
    assert_eq!(forced.analyzed, 1);

    let full = svc.photo_analysis(all[0].id).unwrap();
    assert!(full.blur.is_some() && full.similarity.is_some() && full.perceptual_hash.is_some());
    let listed = svc.analyses(&Filter::default()).unwrap();
    assert_eq!(listed.len(), 4);
    assert!(listed.iter().all(|a| a.similarity.is_none() && a.blur.is_some()), "lists skip embeddings");

    let by_name = |name: &str| all.iter().find(|p| p.file_name == name).unwrap().id;
    let issues = svc.photo_issues(&Filter::default()).unwrap();
    let dark = issues.iter().find(|p| p.photo_id == by_name("d.jpg")).expect("black frame has issues");
    assert!(dark.issues.iter().any(|i| i.kind == IssueKind::Accidental), "{dark:?}");

    let groups = svc.similar_groups(&Filter::default(), &GroupingOptions { max_gap_secs: None, ..Default::default() }).unwrap();
    assert_eq!(groups.len(), 1, "{groups:?}");
    let mut ids = groups[0].photo_ids.clone();
    ids.sort();
    let mut want = vec![by_name("a.jpg"), by_name("b.jpg")];
    want.sort();
    assert_eq!(ids, want);
}
