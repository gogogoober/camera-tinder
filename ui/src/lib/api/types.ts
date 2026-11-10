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
}

/** Which folders to scan (every folder by default). Files already indexed are never read again. */
export interface ScanRequest {
  sourceIds?: string[] | null;
}

export interface ExportOptions {
  includeLikes: boolean;
  includeFavorites: boolean;
}

export interface ExportReport {
  destination: string;
  copied: number;
  alreadyThere: number;
  failed: [string, string][];
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

export interface Plan {
  photos: Photo[];
  /** Photos split by gaps of minutes. */
  moments: Group[];
  /** Runs shot seconds apart (two or more photos). */
  bursts: Group[];
}

export type TagSource = 'manual' | 'calendar' | 'place' | 'gap' | 'image';

export interface Tag {
  id: number;
  name: string;
  startDay: string;
  endDay: string;
  source: TagSource;
}

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
