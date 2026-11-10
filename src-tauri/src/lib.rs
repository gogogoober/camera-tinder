//! Tauri adapter: a thin layer mapping commands/events to `CullerService`.
//! No culling logic lives here — it's all in `cull-core`.

use std::sync::Arc;

use cull_core::*;
use cull_platform_fs::{CopyToDir, FsSource};
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
    blocking(move || svc.add_source(Arc::new(FsSource::new(&path)?))).await
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
async fn scan(app: AppHandle, state: State<'_, AppState>) -> CmdResult<ScanReport> {
    let svc = state.0.clone();
    blocking(move || svc.scan(&TauriSink(app))).await
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
async fn export_keepers(app: AppHandle, state: State<'_, AppState>, dest: String, options: ExportOptions) -> CmdResult<ExportReport> {
    let svc = state.0.clone();
    blocking(move || svc.export(&CopyToDir::new(dest), options, &TauriSink(app))).await
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
            app.manage(AppState(Arc::new(CullerService::open(&db, &cache)?)));
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
            export_keepers
        ])
        .run(tauri::generate_context!())
        .expect("error while running Camera Tinder");
}
