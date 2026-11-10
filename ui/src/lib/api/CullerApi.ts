import type {
  AnalyzeReport,
  AnalyzeRequest,
  AnalyzerInfo,
  CalendarStatus,
  CoreEvent,
  DaySummary,
  Decision,
  ExportOptions,
  ExportReport,
  Filter,
  GroupingOptions,
  OutcomeEntry,
  ResetReport,
  ResetRequest,
  PhotoAnalysis,
  PhotoIssues,
  Photo,
  Plan,
  PreviewCacheStatus,
  ScanReport,
  ScanRequest,
  SimilarGroup,
  SourceInfo,
  Stats,
  TagSuggestion,
} from './types';

/**
 * Everything the UI can ask of the app. Components only ever see this
 * interface — never Tauri — so the backend (Tauri desktop, Tauri iOS, a mock
 * in the browser) can change without touching the UI.
 */
export interface CullerApi {
  /** Platform-specific way to choose where photos come from. Null if cancelled. */
  pickSourceFolder(): Promise<string | null>;
  /** Platform-specific way to choose an export destination. Null if cancelled. */
  pickExportFolder(): Promise<string | null>;

  addFolder(path: string): Promise<SourceInfo>;
  removeSource(id: string): Promise<void>;
  listSources(): Promise<SourceInfo[]>;
  /** Scan folders (all by default). New photos are saved in batches as it goes (`scanSaved` events). */
  scan(request?: ScanRequest): Promise<ScanReport>;
  /** Stop the running scan after the photos being read right now. Finished photos are kept. */
  cancelScan(): Promise<void>;

  queue(filter: Filter): Promise<Photo[]>;
  /** The queue plus its moments and triage (bursts, set-aside photos), for the triage screen. */
  plan(filter: Filter): Promise<Plan>;
  days(): Promise<DaySummary[]>;
  stats(): Promise<Stats>;

  decide(id: number, decision: Decision): Promise<void>;
  /** Record decisions made on suggested photos (call after `decide`). Used to tune the suggestions. */
  logOutcomes(entries: OutcomeEntry[]): Promise<void>;
  /** Testing: forget decisions and/or analysis for every photo taken on these days. Never touches files. */
  resetDays(request: ResetRequest): Promise<ResetReport>;
  /** One decision for many photos, undone together by a single undo. */
  decideMany(ids: number[], decision: Decision): Promise<void>;
  /** Reverts the last action (one photo or a batch); returns the photos, now restored. Empty if nothing to undo. */
  undo(): Promise<Photo[]>;

  /** Guesses from calendar events and runs of shooting days, grouped one level deep.
   *  `dismissed` row ids are left out and the rest is regrouped. */
  suggestTags(dismissed?: string[]): Promise<TagSuggestion[]>;
  /** Whether calendar events can feed tag suggestions. */
  calendarStatus(): Promise<CalendarStatus>;
  /** Asks the OS for calendar access (may show a system prompt) and returns the new status. */
  connectCalendar(): Promise<CalendarStatus>;

  /** Disk used by cached previews, and which sources are still connected. */
  previewCacheStatus(): Promise<PreviewCacheStatus>;
  /** Deletes all cached previews; rejected while any source is connected. Returns bytes freed. */
  clearPreviews(): Promise<number>;

  exportKeepers(dest: string, options: ExportOptions): Promise<ExportReport>;

  // Photo analysis — see docs/photo-analysis.md.
  /** Which engine serves each analysis kind on this platform (null = unavailable). */
  analyzerInfo(): Promise<AnalyzerInfo[]>;
  /** Compute missing/stale results (emits `analysisProgress`). Results persist. */
  analyzePhotos(request?: AnalyzeRequest): Promise<AnalyzeReport>;
  /**
   * Analysis also runs by itself in the background as photos are scanned (`analysisProgress` events).
   * Stop applies before the next step and stays until `resumeAnalysis` or an app restart.
   */
  stopAnalysis(): Promise<void>;
  resumeAnalysis(): Promise<void>;
  analysisStopped(): Promise<boolean>;
  /** Everything stored for one photo, including its similarity embedding. */
  photoAnalysis(id: number): Promise<PhotoAnalysis>;
  /** Stored results for a filter, queue order, without embeddings. */
  photoAnalyses(filter: Filter): Promise<PhotoAnalysis[]>;
  /** Reject reasons (blurry, eyes closed, …) for photos that have any, queue order. */
  photoIssues(filter: Filter): Promise<PhotoIssues[]>;
  /** Near-duplicate groups, each best-first. Needs analyzePhotos first. */
  similarGroups(filter: Filter, options?: GroupingOptions): Promise<SimilarGroup[]>;

  /** URL for a display-sized preview, usable directly in <img src>. */
  previewUrl(id: number, edge?: number): string;
  /** Subscribe to progress events. Returns an unsubscribe function. */
  onEvent(cb: (e: CoreEvent) => void): Promise<() => void>;
}
