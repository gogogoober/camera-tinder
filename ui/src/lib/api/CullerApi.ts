import type {
  CalendarStatus,
  CoreEvent,
  DaySummary,
  Decision,
  ExportOptions,
  ExportReport,
  Filter,
  Photo,
  Plan,
  PreviewCacheStatus,
  ScanReport,
  SourceInfo,
  Stats,
  Tag,
  TagSource,
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
  scan(): Promise<ScanReport>;

  queue(filter: Filter): Promise<Photo[]>;
  /** The queue plus its moments and bursts, for the cull screen. */
  plan(filter: Filter): Promise<Plan>;
  days(): Promise<DaySummary[]>;
  stats(): Promise<Stats>;

  decide(id: number, decision: Decision): Promise<void>;
  /** One decision for many photos, undone together by a single undo. */
  decideMany(ids: number[], decision: Decision): Promise<void>;
  /** Reverts the last action (one photo or a batch); returns the photos, now restored. Empty if nothing to undo. */
  undo(): Promise<Photo[]>;

  tags(): Promise<Tag[]>;
  addTag(name: string, startDay: string, endDay: string, source?: TagSource): Promise<Tag>;
  removeTag(id: number): Promise<void>;
  /** Guesses from calendar events and runs of shooting days. */
  suggestTags(): Promise<TagSuggestion[]>;
  /** Whether calendar events can feed tag suggestions. */
  calendarStatus(): Promise<CalendarStatus>;
  /** Asks the OS for calendar access (may show a system prompt) and returns the new status. */
  connectCalendar(): Promise<CalendarStatus>;

  /** Disk used by cached previews, and which sources are still connected. */
  previewCacheStatus(): Promise<PreviewCacheStatus>;
  /** Deletes all cached previews; rejected while any source is connected. Returns bytes freed. */
  clearPreviews(): Promise<number>;

  exportKeepers(dest: string, options: ExportOptions): Promise<ExportReport>;

  /** URL for a display-sized preview, usable directly in <img src>. */
  previewUrl(id: number, edge?: number): string;
  /** Subscribe to progress events. Returns an unsubscribe function. */
  onEvent(cb: (e: CoreEvent) => void): Promise<() => void>;
}
