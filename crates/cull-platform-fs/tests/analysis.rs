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

#[test]
fn results_of_a_replaced_engine_are_not_read_but_stay_stored() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    write(&card.join("a.jpg"), |x, y| scene(x, y, 0));
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    svc.analyze(&AnalyzeRequest::default(), &NullSink).unwrap();
    let id = svc.queue(&Filter::default()).unwrap()[0].id;
    assert!(svc.photo_analysis(id).unwrap().blur.is_some());

    // Remove the blur engine: its stored row is ignored, not deleted.
    let mut analyzers = cull_core::analysis::Analyzers::rust();
    analyzers.blur = None;
    svc.set_analyzers(analyzers);
    assert!(svc.photo_analysis(id).unwrap().blur.is_none());
    assert!(svc.photo_analysis(id).unwrap().exposure.is_some(), "other results are still read");
    assert_eq!(svc.analyze(&AnalyzeRequest::default(), &NullSink).unwrap().up_to_date, 1, "nothing to recompute");

    // Put it back: the old row is current again, with no recomputation.
    svc.set_analyzers(cull_core::analysis::Analyzers::rust());
    assert!(svc.photo_analysis(id).unwrap().blur.is_some());
    assert_eq!(svc.analyze(&AnalyzeRequest::default(), &NullSink).unwrap().up_to_date, 1);
}

// ---- background worker ----------------------------------------------------------

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !ok() {
        assert!(start.elapsed() < std::time::Duration::from_secs(30), "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn card_of_four(card: &std::path::Path) {
    for (i, name) in ["a", "b", "c", "d"].iter().enumerate() {
        write(&card.join(format!("{name}.jpg")), |x, y| scene(x, y, i as u32 * 40));
    }
}

fn all_analysed(svc: &CullerService) -> bool {
    let a = svc.analyses(&Filter::default()).unwrap();
    a.len() == 4 && a.iter().all(|x| x.blur.is_some() && x.perceptual_hash.is_some())
}

#[test]
fn the_worker_analyses_photos_as_the_scan_saves_them() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    card_of_four(&card);
    let svc = Arc::new(CullerService::in_memory(&tmp.path().join("cache")).unwrap());
    svc.start_analysis_worker(Arc::new(NullSink));
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    wait_until("analysis of every photo", || all_analysed(&svc));
}

#[test]
fn stop_is_sticky_until_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    card_of_four(&card);
    let svc = Arc::new(CullerService::in_memory(&tmp.path().join("cache")).unwrap());
    svc.stop_analysis();
    assert!(svc.analysis_stopped());
    svc.start_analysis_worker(Arc::new(NullSink));
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(svc.analyses(&Filter::default()).unwrap().iter().all(|a| a.blur.is_none()), "stopped: nothing analysed, even after new photos");
    let r = svc.analyze_pending(&NullSink).unwrap();
    assert_eq!(r.analyzed, 0, "a manual run honours Stop too");

    svc.resume_analysis();
    assert!(!svc.analysis_stopped());
    wait_until("analysis after resume", || all_analysed(&svc));
}

#[test]
fn a_restarted_app_finishes_unfinished_analysis_without_redoing_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    card_of_four(&card);
    let (db, cache) = (tmp.path().join("db").join("app.db"), tmp.path().join("cache"));
    let first_id = {
        let svc = CullerService::open(&db, &cache).unwrap();
        svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
        svc.scan(&Default::default(), &NullSink).unwrap();
        let first = svc.queue(&Filter::default()).unwrap()[0].id;
        svc.analyze(&AnalyzeRequest { photo_ids: Some(vec![first]), ..Default::default() }, &NullSink).unwrap();
        first
    };
    let svc = Arc::new(CullerService::open(&db, &cache).unwrap());
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.start_analysis_worker(Arc::new(NullSink));
    wait_until("the first pass after restart", || all_analysed(&svc));
    let r = svc.analyze_pending(&NullSink).unwrap();
    assert_eq!((r.analyzed, r.up_to_date), (0, 4), "{r:?} {first_id}");
}

#[test]
fn asking_for_one_kind_computes_only_the_step_that_writes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    write(&card.join("a.jpg"), |x, y| scene(x, y, 0));
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();
    let id = svc.queue(&Filter::default()).unwrap()[0].id;

    svc.analyze(&AnalyzeRequest { kinds: vec![AnalysisKind::Blur], ..Default::default() }, &NullSink).unwrap();
    let a = svc.photo_analysis(id).unwrap();
    assert!(a.blur.is_some() && a.exposure.is_none() && a.perceptual_hash.is_none(), "only blur was computed");

    // The rest is still missing, so a full run computes it and leaves blur alone.
    let r = svc.analyze(&AnalyzeRequest::default(), &NullSink).unwrap();
    assert_eq!((r.analyzed, r.up_to_date), (1, 0));
    assert!(svc.photo_analysis(id).unwrap().exposure.is_some());
}

/// A blur engine that crashes, like a decoder bug on a damaged photo would.
struct CrashingBlur;
impl cull_core::analysis::BlurAnalyzer for CrashingBlur {
    fn implementation(&self) -> &'static str {
        "test.crashing_blur"
    }
    fn blur(&self, _: &cull_core::analysis::AnalysisImage) -> cull_core::Result<cull_core::analysis::Blur> {
        panic!("engine bug");
    }
}

#[test]
fn a_crashing_engine_fails_its_photos_but_not_the_pass_or_the_worker() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    card_of_four(&card);
    let svc = Arc::new(CullerService::in_memory(&tmp.path().join("cache")).unwrap());
    let mut broken = cull_core::analysis::Analyzers::rust();
    broken.blur = Some(Arc::new(CrashingBlur));
    svc.set_analyzers(broken);
    svc.start_analysis_worker(Arc::new(NullSink));
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();

    let r = svc.analyze(&AnalyzeRequest::default(), &NullSink).unwrap();
    assert_eq!(r.failed.len(), 4, "every photo reported, none lost: {r:?}");
    std::thread::sleep(std::time::Duration::from_millis(500)); // the worker met the crash too

    // Fix the engine: the same worker (still alive) finishes the job.
    svc.set_analyzers(cull_core::analysis::Analyzers::rust());
    svc.wake_analysis();
    wait_until("analysis after the engine was fixed", || all_analysed(&svc));
}

#[test]
fn adding_a_folder_wakes_the_worker_for_photos_scanned_in_an_earlier_session() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    card_of_four(&card);
    let (db, cache) = (tmp.path().join("db").join("app.db"), tmp.path().join("cache"));
    {
        let first = CullerService::open(&db, &cache).unwrap();
        first.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
        first.scan(&Default::default(), &NullSink).unwrap();
    }
    // A new launch: the worker's first pass sees no sources yet, so nothing happens until one is added.
    let svc = Arc::new(CullerService::open(&db, &cache).unwrap());
    svc.start_analysis_worker(Arc::new(NullSink));
    std::thread::sleep(std::time::Duration::from_millis(300));
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    wait_until("analysis after add_source", || all_analysed(&svc));
}

#[test]
fn failed_photos_go_to_the_error_log_once_not_on_every_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let card = tmp.path().join("card");
    card_of_four(&card);
    let svc = CullerService::in_memory(&tmp.path().join("cache")).unwrap();
    assert_eq!(svc.error_log(10), ErrorLogView::default(), "no file until the platform sets one");
    svc.set_error_log(tmp.path().join("logs").join("errors.log"));
    let mut broken = cull_core::analysis::Analyzers::rust();
    broken.blur = Some(Arc::new(CrashingBlur));
    svc.set_analyzers(broken);
    svc.add_source(Arc::new(FsSource::new(&card).unwrap())).unwrap();
    svc.scan(&Default::default(), &NullSink).unwrap();

    svc.analyze(&AnalyzeRequest::default(), &NullSink).unwrap();
    svc.analyze(&AnalyzeRequest::default(), &NullSink).unwrap(); // a retry: nothing new is logged
    let log = svc.error_log(100);
    assert!(log.path.unwrap().ends_with("errors.log"));
    assert_eq!(log.lines.len(), 4, "one line per failed photo: {:?}", log.lines);
    assert!(log.lines.iter().all(|l| l.contains("[analysis]") && l.contains("analysis crashed on this photo") && l.contains(".jpg")), "{:?}", log.lines);
}
