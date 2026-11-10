import type { IssueKind } from './analysisTypes';
// Mirrors the serde types in crates/cull-core/src/model.rs.

export type Decision = 'like' | 'dislike' | 'favorite' | 'hate';
export type MediaKind = 'jpeg' | 'raw';
export type StatusFilter = 'undecided' | 'all' | 'keepers' | 'rejects';

export interface Photo {
  id: number;
  fingerprint: string;
  sourceId: string;
  relPath: string;
  fileName: string;
  kind: MediaKind;
  size: number;
  capturedAt: string | null; // "YYYY-MM-DDTHH:MM:SS"
  camera: string | null;
  decision: Decision | null;
}

export interface SourceInfo {
  id: string;
  name: string;
  photoCount: number;
}

export interface Stats {
  total: number;
  undecided: number;
  like: number;
  dislike: number;
  favorite: number;
  hate: number;
}

export interface DaySummary {
  day: string | null;
  stats: Stats;
}

export interface Filter {
  /** Only these days ("YYYY-MM-DD", or "unknown" for no date); null/absent = every day, [] = none. */
  days?: string[] | null;
  status: StatusFilter;
}

export interface ScanReport {
  found: number;
  new: number;
  skippedUnsupported: number;
  /** Videos and other files the app ignores; not exported. */
  otherFiles: number;
  failed: [string, string][];
  /** Stopped before the end; everything finished is kept. */
  cancelled: boolean;
  /** Another scan was running: it does this one right after, and this call did nothing. */
  queued: boolean;
}

/** A folder or card the database knows, in the session or not (Advanced → Library). */
export interface LibrarySource {
  id: string;
  name: string;
  photoCount: number;
  inSession: boolean;
}

/** What forgetting a folder removed. */
export interface ForgetReport {
  photos: number;
  previewBytes: number;
}

/** Which folders to scan (every folder by default). Files already indexed are never read again. */
export interface ScanRequest {
  sourceIds?: string[] | null;
}

/** Which decisions to export, optionally only some days. Always the original files. */
export interface ExportOptions {
  includeLikes: boolean;
  includeFavorites: boolean;
  /** Nope (stored as `dislike`). */
  includeDislikes?: boolean;
  includeHates?: boolean;
  /** Also export photos exported before (any earlier export). Omitted = true; the Export screen sends false by default. */
  includeExported?: boolean;
  /** Only photos taken on these days (`YYYY-MM-DD`, `"unknown"`); null/absent = every day. */
  days?: string[] | null;
}

/** A saved name for a set of days ("Japan trip"). Tags may overlap. */
export interface SavedTag {
  id: number;
  name: string;
  days: string[];
}

export interface ExportReport {
  destination: string;
  copied: number;
  alreadyThere: number;
  failed: [string, string][];
  /** Copies succeeded but could not be recorded as exported (they may be copied again next time). */
  notMarked?: string | null;
}

export type CoreEvent =
  | { type: 'scanProgress'; sourceId: string; done: number; total: number }
  /** A batch of new photos was saved mid-scan: refresh what's shown. */
  | { type: 'scanSaved'; sourceId: string; saved: number }
  | { type: 'exportProgress'; done: number; total: number }
  | { type: 'analysisProgress'; done: number; total: number };

/** Half-open index range [start, end) into a plan's photo list. */
export interface Group {
  start: number;
  end: number;
}

export type Question = 'judge' | 'pick' | 'sweep';
export type GroupKind = 'burst' | 'similar';

export interface TriageGroup {
  /** The group's earliest photo; the same for every member. */
  id: number;
  /** burst = frames seconds apart; similar = the same scene again, minutes apart. */
  kind: GroupKind;
  size: number;
  /** 0 = best frame. */
  rank: number;
  /** ★: the best frame, and it has no reject-level issue. */
  suggested: boolean;
}

/** What to ask about one photo (see docs/smart-triage.md). */
export interface TriageItem {
  photoId: number;
  question: Question;
  group: TriageGroup | null;
  /** Why it is set aside (sweep) or what is wrong with this frame (pick), worst first. */
  reasons: IssueKind[];
  /** Other issues found: badges only. */
  hints: IssueKind[];
  score: number | null;
  /** False until the photo has analysis results; then it is just a judge. */
  analyzed: boolean;
}

/** Where a suggested photo was decided. */
/** `bulk` = a whole moment or drag-selected range decided at once (not a per-photo judgement). */
export type Place = 'timeline' | 'openedEarly' | 'rescue' | 'bulk';

/** One decision on a photo triage had an opinion about; logged to measure the suggestions. */
export interface OutcomeEntry {
  item: TriageItem;
  planVersion: string;
  place: Place;
  decision: Decision;
}

export interface Plan {
  photos: Photo[];
  /** One item per photo, same order. */
  triage: TriageItem[];
  triageVersion: string;
  /** Photos split by gaps of minutes. */
  moments: Group[];
}

export type TagSource = 'manual' | 'calendar' | 'place' | 'gap' | 'image';

export interface TagSuggestion {
  /** Stable row id; pass it to `suggestTags(dismissed)` to leave the row out. */
  id: string;
  name: string;
  startDay: string;
  endDay: string;
  source: TagSource;
  reason: string;
  photoCount: number;
  /** More than one name = several multi-day events on exactly the same days (`name` joins them). */
  names: string[];
  /** Events inside this one's days; one level only. */
  children: TagSuggestion[];
}

/** `unavailable` = this platform has no calendar source. */
export type CalendarStatus = 'connected' | 'notDetermined' | 'denied' | 'unavailable';

export interface PreviewCacheStatus {
  files: number;
  bytes: number;
  /** Photo sources reachable right now. Clearing is refused until this is empty. */
  connectedSources: string[];
}

export * from './analysisTypes';

/** The background error log (developer view). */
export interface ErrorLogView {
  /** The file, or null when errors only go to the terminal (e.g. the mock backend). */
  path: string | null;
  /** Last lines, oldest first. */
  lines: string[];
}

/** Forget what was recorded for the photos of some days in this session's folders and cards (testing). Files and previews are kept. */
export interface ResetRequest {
  /** YYYY-MM-DD capture days, or "unknown" for photos without a date. Empty = nothing. */
  days: string[];
  /** Decisions, their history and logged outcomes. */
  decisions: boolean;
  /** Analysis results (computed again in the background). */
  analysis: boolean;
}

export interface ResetReport {
  photos: number;
  decisions: number;
  analysis: number;
}
