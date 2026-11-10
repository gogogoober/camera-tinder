//! Tauri adapter: a thin layer mapping commands/events to `CullerService`.
//! No culling logic lives here — it's all in `cull-core`.

use std::sync::Arc;

use cull_core::*;
use cull_platform_fs::{CopyToDir, FsSource, is_external_and_connected};
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
fn remove_source(state: State<'_, AppState>, id: String) {
    state.0.remove_source(&id);
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
async fn tags(state: State<'_, AppState>) -> CmdResult<Vec<Tag>> {
    let svc = state.0.clone();
    blocking(move || svc.tags()).await
}

#[tauri::command]
async fn add_tag(state: State<'_, AppState>, name: String, start_day: String, end_day: String, source: Option<TagSource>) -> CmdResult<Tag> {
    let svc = state.0.clone();
    blocking(move || svc.add_tag(&name, &start_day, &end_day, source.unwrap_or(TagSource::Manual))).await
}

#[tauri::command]
async fn remove_tag(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    let svc = state.0.clone();
    blocking(move || svc.remove_tag(id)).await
}

#[tauri::command]
async fn suggest_tags(state: State<'_, AppState>) -> CmdResult<Vec<TagSuggestion>> {
    let svc = state.0.clone();
    blocking(move || svc.suggest_tags()).await
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
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let db = app.path().app_data_dir()?.join("camera-tinder.db");
            let cache = app.path().app_cache_dir()?.join("previews");
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
            list_sources,
            scan,
            cancel_scan,
            queue,
            plan,
            days,
            stats,
            decide,
            decide_many,
            undo,
            tags,
            add_tag,
            remove_tag,
            suggest_tags,
            calendar_status,
            preview_cache_status,
            clear_previews,
            connect_calendar,
            export_keepers,
            analyzer_info,
            analyze_photos,
            photo_analysis,
            photo_analyses,
            photo_issues,
            similar_groups
        ])
        .run(tauri::generate_context!())
        .expect("error while running Camera Tinder");
}
