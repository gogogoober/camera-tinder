//! Tauri adapter: a thin layer mapping commands/events to `CullerService`.
//! No culling logic lives here — it's all in `cull-core`.

use std::sync::Arc;

use cull_core::*;
use cull_core::offload::{FolderChoice, OffloadPlan, OffloadReport};
use cull_platform_fs::{CopyToDir, FsLibrary, FsSource, is_external_and_connected};
use tauri::http::{Response, StatusCode, header};
use tauri::{AppHandle, Emitter, Manager, State};

struct AppState(Arc<CullerService>);

/// Forwards core progress events to the webview as `core-event`.
struct TauriSink(AppHandle);
impl EventSink for TauriSink {
    fn emit(&self, event: CoreEvent) {
        let _ = self.0.emit("core-event", event);
    }
}

type CmdResult<T> = std::result::Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Run blocking core work off the main thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> cull_core::Result<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(err)?.map_err(err)
}

#[tauri::command]
async fn add_folder(state: State<'_, AppState>, path: String) -> CmdResult<SourceInfo> {
    let svc = state.0.clone();
    // A picked folder that contains the app's own folders (home folder, whole disk) never lists them.
    blocking(move || svc.add_source(Arc::new(FsSource::new(&path)?.excluding(svc.app_dirs())))).await
}

#[tauri::command]
fn remove_source(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.0.remove_source(&id).map_err(err)
}

/// Forget a folder: out of the session, its photos' rows and previews deleted. Never touches the folder.
#[tauri::command]
async fn forget_source(state: State<'_, AppState>, id: String) -> CmdResult<ForgetReport> {
    let svc = state.0.clone();
    blocking(move || svc.forget_source(&id)).await
}

/// Every folder or card the database knows (Advanced → Library).
#[tauri::command]
async fn library(state: State<'_, AppState>) -> CmdResult<Vec<LibrarySource>> {
    let svc = state.0.clone();
    blocking(move || svc.library()).await
}

#[tauri::command]
async fn list_sources(state: State<'_, AppState>) -> CmdResult<Vec<SourceInfo>> {
    let svc = state.0.clone();
    blocking(move || svc.sources()).await
}

#[tauri::command]
async fn scan(app: AppHandle, state: State<'_, AppState>, request: Option<ScanRequest>) -> CmdResult<ScanReport> {
    let svc = state.0.clone();
    let request = request.unwrap_or_default();
    blocking(move || svc.scan(&request, &TauriSink(app))).await
}

/// Stop the running scan after the photos being read right now; finished photos are kept.
#[tauri::command]
fn cancel_scan(state: State<'_, AppState>) {
    state.0.cancel_scan();
}

#[tauri::command]
async fn queue(state: State<'_, AppState>, filter: Filter) -> CmdResult<Vec<Photo>> {
    let svc = state.0.clone();
    blocking(move || svc.queue(&filter)).await
}

#[tauri::command]
async fn plan(state: State<'_, AppState>, filter: Filter) -> CmdResult<Plan> {
    let svc = state.0.clone();
    blocking(move || svc.plan(&filter)).await
}

#[tauri::command]
async fn days(state: State<'_, AppState>) -> CmdResult<Vec<DaySummary>> {
    let svc = state.0.clone();
    blocking(move || svc.days()).await
}

#[tauri::command]
async fn stats(state: State<'_, AppState>) -> CmdResult<Stats> {
    let svc = state.0.clone();
    blocking(move || svc.stats()).await
}

#[tauri::command]
async fn decide(state: State<'_, AppState>, id: PhotoId, decision: Decision) -> CmdResult<()> {
    let svc = state.0.clone();
    blocking(move || svc.decide(id, decision)).await
}

#[tauri::command]
async fn log_outcomes(state: State<'_, AppState>, entries: Vec<cull_core::triage::OutcomeEntry>) -> CmdResult<()> {
    let svc = state.0.clone();
    blocking(move || svc.log_outcomes(&entries)).await
}

/// Forget decisions and/or analysis for the photos of some days (testing; never touches files).
#[tauri::command]
async fn reset_days(state: State<'_, AppState>, request: ResetRequest) -> CmdResult<ResetReport> {
    let svc = state.0.clone();
    blocking(move || svc.reset_days(&request)).await
}

#[tauri::command]
async fn decide_many(state: State<'_, AppState>, ids: Vec<PhotoId>, decision: Decision) -> CmdResult<()> {
    let svc = state.0.clone();
    blocking(move || svc.decide_many(&ids, decision)).await
}

#[tauri::command]
async fn undo(state: State<'_, AppState>) -> CmdResult<Vec<Photo>> {
    let svc = state.0.clone();
    blocking(move || svc.undo()).await
}

#[tauri::command]
async fn suggest_tags(state: State<'_, AppState>, dismissed: Option<Vec<String>>) -> CmdResult<Vec<TagSuggestion>> {
    let svc = state.0.clone();
    blocking(move || svc.suggest_tags(&dismissed.unwrap_or_default())).await
}

#[tauri::command]
async fn preview_cache_status(state: State<'_, AppState>) -> CmdResult<PreviewCacheStatus> {
    let svc = state.0.clone();
    blocking(move || svc.preview_cache_status(&is_external_and_connected)).await
}

/// Refused (with the names to unplug) while any card or external drive with photos is connected.
#[tauri::command]
async fn clear_previews(state: State<'_, AppState>) -> CmdResult<u64> {
    let svc = state.0.clone();
    blocking(move || svc.clear_previews(&is_external_and_connected)).await
}

/// "connected" | "notDetermined" | "denied" | "unavailable" (not macOS).
#[tauri::command]
fn calendar_status() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        use cull_platform_apple::{Access, CalendarAccess};
        return match CalendarAccess::status() {
            Access::Granted => "connected",
            Access::NotDetermined => "notDetermined",
            Access::Denied => "denied",
        };
    }
    #[allow(unreachable_code)]
    "unavailable"
}

/// Asks macOS for calendar access (first time shows the system prompt) and starts using it.
#[tauri::command]
async fn connect_calendar(state: State<'_, AppState>) -> CmdResult<&'static str> {
    #[cfg(target_os = "macos")]
    {
        use cull_platform_apple::{Access, CalendarAccess, EventKitCalendar};
        let svc = state.0.clone();
        let granted = tauri::async_runtime::spawn_blocking(CalendarAccess::request).await.map_err(err)?.map_err(err)?;
        if granted == Access::Granted {
            svc.set_calendar(EventKitCalendar::new().map(|c| Arc::new(c) as Arc<dyn CalendarSource>));
        }
        return Ok(calendar_status());
    }
    #[allow(unreachable_code)]
    {
        let _ = state;
        Ok("unavailable")
    }
}

#[tauri::command]
async fn export_keepers(app: AppHandle, state: State<'_, AppState>, dest: String, options: ExportOptions) -> CmdResult<ExportReport> {
    let svc = state.0.clone();
    blocking(move || {
        // FsSource ids are canonical folder paths: never write into any folder ever culled from.
        let roots = svc.known_source_ids()?;
        svc.export(&CopyToDir::new(dest).protecting(roots), options, &TauriSink(app))
    })
    .await
}

#[tauri::command]
async fn export_count(state: State<'_, AppState>, options: ExportOptions) -> CmdResult<usize> {
    let svc = state.0.clone();
    blocking(move || svc.export_count(options)).await
}

// ---- offload: back up a card to a drive (docs/offload.md) --------------

/// Read the card and the drive and propose folders. Writes nothing.
#[tauri::command]
async fn offload_plan(app: AppHandle, state: State<'_, AppState>, card: String, library: String, full_check: bool) -> CmdResult<OffloadPlan> {
    let svc = state.0.clone();
    blocking(move || {
        let card_src = FsSource::new(&card)?;
        let card_root = card_src.root().to_string_lossy().into_owned();
        // Refuses a drive on the card's volume (nothing is ever written onto a card).
        let lib = FsLibrary::new(&library, card_src.root())?;
        let reader = FsSource::new(&library)?.excluding(svc.app_dirs());
        svc.offload_plan(Arc::new(card_src), card_root, Arc::new(reader), Arc::new(lib), full_check, &TauriSink(app))
    })
    .await
}

/// The current plan with folder choices applied. Reads nothing.
#[tauri::command]
async fn offload_layout(state: State<'_, AppState>, choices: Vec<FolderChoice>) -> CmdResult<OffloadPlan> {
    let svc = state.0.clone();
    blocking(move || svc.offload_layout(&choices)).await
}

/// Copy the plan's new files, read each back from the drive, and report. Keeps the Mac awake meanwhile.
#[tauri::command]
async fn offload_start(app: AppHandle, state: State<'_, AppState>, choices: Vec<FolderChoice>, read_twice: bool) -> CmdResult<OffloadReport> {
    let svc = state.0.clone();
    blocking(move || {
        // Keep the Mac awake (not the display) for the duration of the copy.
        #[cfg(target_os = "macos")]
        let _awake = cull_platform_apple::power::KeepAwake::new("Backing up a card");
        svc.offload_start(&choices, read_twice, &TauriSink(app))
    })
    .await
}

/// Stop planning or copying; verified files stay, the one in progress is removed.
#[tauri::command]
fn offload_stop(state: State<'_, AppState>) {
    state.0.offload_stop();
}

// ---- saved tags (a name for a set of days) ----------------------------

#[tauri::command]
async fn list_tags(state: State<'_, AppState>) -> CmdResult<Vec<SavedTag>> {
    let svc = state.0.clone();
    blocking(move || svc.list_tags()).await
}

#[tauri::command]
async fn save_tag(state: State<'_, AppState>, name: String, days: Vec<String>) -> CmdResult<SavedTag> {
    let svc = state.0.clone();
    blocking(move || svc.save_tag(&name, &days)).await
}

#[tauri::command]
async fn delete_tag(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    let svc = state.0.clone();
    blocking(move || svc.delete_tag(id)).await
}

// ---- photo analysis (see docs/photo-analysis.md) ----------------------

#[tauri::command]
fn analyzer_info(state: State<'_, AppState>) -> Vec<analysis::AnalyzerInfo> {
    state.0.analyzer_info()
}

/// Compute missing/stale analysis results. Emits `analysisProgress` core events.
#[tauri::command]
async fn analyze_photos(app: AppHandle, state: State<'_, AppState>, request: analysis::AnalyzeRequest) -> CmdResult<analysis::AnalyzeReport> {
    let svc = state.0.clone();
    blocking(move || svc.analyze(&request, &TauriSink(app))).await
}

/// Stop background analysis before its next step. Stays stopped until `resume_analysis` or a restart.
#[tauri::command]
fn stop_analysis(state: State<'_, AppState>) {
    state.0.stop_analysis();
}

#[tauri::command]
fn resume_analysis(state: State<'_, AppState>) {
    state.0.resume_analysis();
}

/// The background error log's path and last lines (developer view in the Advanced drawer).
#[tauri::command]
fn error_log(state: State<'_, AppState>, max_lines: Option<usize>) -> ErrorLogView {
    state.0.error_log(max_lines.unwrap_or(200))
}

#[tauri::command]
fn analysis_stopped(state: State<'_, AppState>) -> bool {
    state.0.analysis_stopped()
}

#[tauri::command]
async fn photo_analysis(state: State<'_, AppState>, id: PhotoId) -> CmdResult<analysis::PhotoAnalysis> {
    let svc = state.0.clone();
    blocking(move || svc.photo_analysis(id)).await
}

#[tauri::command]
async fn photo_analyses(state: State<'_, AppState>, filter: Filter) -> CmdResult<Vec<analysis::PhotoAnalysis>> {
    let svc = state.0.clone();
    blocking(move || svc.analyses(&filter)).await
}

#[tauri::command]
async fn photo_issues(state: State<'_, AppState>, filter: Filter) -> CmdResult<Vec<analysis::PhotoIssues>> {
    let svc = state.0.clone();
    blocking(move || svc.photo_issues(&filter)).await
}

#[tauri::command]
async fn similar_groups(state: State<'_, AppState>, filter: Filter, options: Option<analysis::GroupingOptions>) -> CmdResult<Vec<analysis::SimilarGroup>> {
    let svc = state.0.clone();
    blocking(move || svc.similar_groups(&filter, &options.unwrap_or_default())).await
}

/// `preview://localhost/<photoId>?edge=<px>` → cached, downscaled JPEG.
/// Images go over a URL scheme, never through JSON commands, so they stay fast.
fn handle_preview(app: &AppHandle, uri: &tauri::http::Uri) -> Response<Vec<u8>> {
    let id: Option<PhotoId> = uri.path().trim_start_matches('/').parse().ok();
    let edge: u32 = uri
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("edge=")))
        .and_then(|v| v.parse().ok())
        .unwrap_or(2048);
    let result = match id {
        Some(id) => app.state::<AppState>().0.preview(id, edge),
        None => Err(CoreError::Other(format!("bad preview url: {uri}"))),
    };
    let builder = Response::builder().header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
    match result {
        Ok(bytes) => builder
            .header(header::CONTENT_TYPE, "image/jpeg")
            .header(header::CACHE_CONTROL, "max-age=31536000, immutable")
            .body(bytes),
        Err(e) => builder.status(StatusCode::NOT_FOUND).body(e.to_string().into_bytes()),
    }
    .expect("valid response")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// The app used to be "Camera Tinder" (identifier `app.cameratinder.desktop`, `camera-tinder.db`). On the first
/// launch under the new name, move its database (with its `-wal`/`-shm` files) and preview cache over, so decisions
/// and previews survive. Runs only while the new database doesn't exist; each file moves only if its target is free,
/// and the database moves last, so an interrupted move finishes on the next launch. Failures are logged, not fatal.
fn move_data_from_camera_tinder(db: &std::path::Path, cache: &std::path::Path) {
    const OLD_ID: &str = "app.cameratinder.desktop";
    let (Some(data_dir), Some(cache_dir)) = (db.parent(), cache.parent()) else { return };
    let (Some(data_root), Some(cache_root)) = (data_dir.parent(), cache_dir.parent()) else { return };
    let old_data = data_root.join(OLD_ID);
    if db.exists() || !old_data.join("camera-tinder.db").exists() {
        return;
    }
    let mut moves: Vec<(std::path::PathBuf, std::path::PathBuf)> = vec![(cache_root.join(OLD_ID).join("previews"), cache.to_path_buf())];
    for suffix in ["-wal", "-shm", ""] {
        moves.push((old_data.join(format!("camera-tinder.db{suffix}")), data_dir.join(format!("darkroom.db{suffix}"))));
    }
    for (from, to) in moves {
        if !from.exists() || to.exists() {
            continue;
        }
        let moved = to.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::rename(&from, &to));
        if let Err(e) = moved {
            eprintln!("Couldn't move {} to {}: {e}", from.display(), to.display());
        }
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let db = app.path().app_data_dir()?.join("darkroom.db");
            let cache = app.path().app_cache_dir()?.join("previews");
            move_data_from_camera_tinder(&db, &cache);
            let svc = Arc::new(CullerService::open(&db, &cache)?);
            // Only reconnect if the user already said yes; never prompt at launch.
            #[cfg(target_os = "macos")]
            if let Some(cal) = cull_platform_apple::EventKitCalendar::new() {
                svc.set_calendar(Some(Arc::new(cal)));
            }
            // Apple Vision engines for faces, eyes, aesthetics, similarity… (Rust ones elsewhere).
            #[cfg(target_os = "macos")]
            svc.set_analyzers(cull_platform_apple::vision::analyzers());
            // ImageIO previews: decodes straight at preview size (several times faster than the portable path).
            #[cfg(target_os = "macos")]
            svc.set_preview_maker(Arc::new(cull_platform_apple::imageio::ImageIoPreviewMaker));
            // Background errors (analysis) go to a rotating file in the app's log folder, for troubleshooting.
            svc.set_error_log(app.path().app_log_dir()?.join("errors.log"));
            // Reconnect the folders that were in the session last time, when they're there (a card may be out).
            for id in svc.saved_session_sources()? {
                if let Ok(src) = FsSource::new(&id) {
                    if src.id() == id {
                        svc.add_source(Arc::new(src.excluding(svc.app_dirs())))?;
                    }
                }
            }
            // Analyse every photo in the background as the scan saves it (and resume unfinished work now).
            svc.start_analysis_worker(Arc::new(TauriSink(app.handle().clone())));
            app.manage(AppState(svc));
            Ok(())
        })
        .register_asynchronous_uri_scheme_protocol("preview", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            let uri = request.uri().clone();
            tauri::async_runtime::spawn_blocking(move || responder.respond(handle_preview(&app, &uri)));
        })
        .invoke_handler(tauri::generate_handler![
            add_folder,
            remove_source,
            forget_source,
            library,
            list_sources,
            scan,
            cancel_scan,
            queue,
            plan,
            days,
            stats,
            decide,
            decide_many,
            reset_days,
            log_outcomes,
            undo,
            suggest_tags,
            calendar_status,
            preview_cache_status,
            clear_previews,
            connect_calendar,
            export_keepers,
            export_count,
            list_tags,
            save_tag,
            delete_tag,
            analyzer_info,
            analyze_photos,
            stop_analysis,
            resume_analysis,
            analysis_stopped,
            error_log,
            photo_analysis,
            photo_analyses,
            photo_issues,
            similar_groups,
            offload_plan,
            offload_layout,
            offload_start,
            offload_stop
        ])
        .run(tauri::generate_context!())
        .expect("error while running Darkroom");
}

#[cfg(test)]
mod tests {
    use std::fs;

    #[test]
    fn camera_tinder_data_moves_once_and_never_overwrites() {
        let root = std::env::temp_dir().join(format!("darkroom-move-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (old_data, old_cache) = (root.join("data/app.cameratinder.desktop"), root.join("cache/app.cameratinder.desktop/previews"));
        fs::create_dir_all(&old_data).unwrap();
        fs::create_dir_all(&old_cache).unwrap();
        for f in ["camera-tinder.db", "camera-tinder.db-wal", "camera-tinder.db-shm"] {
            fs::write(old_data.join(f), f).unwrap();
        }
        fs::write(old_cache.join("a_256.jpg"), "thumb").unwrap();
        let (db, cache) = (root.join("data/app.darkroom.desktop/darkroom.db"), root.join("cache/app.darkroom.desktop/previews"));

        super::move_data_from_camera_tinder(&db, &cache);
        assert_eq!(fs::read_to_string(&db).unwrap(), "camera-tinder.db");
        assert_eq!(fs::read_to_string(db.with_file_name("darkroom.db-wal")).unwrap(), "camera-tinder.db-wal");
        assert_eq!(fs::read_to_string(cache.join("a_256.jpg")).unwrap(), "thumb");
        assert!(!old_data.join("camera-tinder.db").exists());

        // A new old database later (e.g. the old build ran again) is left alone: the new one already exists.
        fs::write(old_data.join("camera-tinder.db"), "newer").unwrap();
        super::move_data_from_camera_tinder(&db, &cache);
        assert_eq!(fs::read_to_string(&db).unwrap(), "camera-tinder.db");
        fs::remove_dir_all(&root).unwrap();
    }
}
