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
  day: string | null; // "YYYY-MM-DD" (start of a range), "unknown", or null for all
  dayTo?: string | null; // inclusive end of the range
  status: StatusFilter;
}

export interface ScanReport {
  found: number;
  new: number;
  skippedUnsupported: number;
  /** Videos and other files the app ignores; not exported. */
  otherFiles: number;
  failed: [string, string][];
  /** Folders skipped because they hold the same files as at their last complete scan. */
  unchangedSources: number;
  /** Stopped before the end; everything finished is kept. */
  cancelled: boolean;
}

/** Which folders to scan. Defaults: every folder, skipping ones that look unchanged. */
export interface ScanRequest {
  sourceIds?: string[] | null;
  /** Check every file even if the folder looks unchanged (already-indexed files are still not read). */
  force?: boolean;
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
  name: string;
  startDay: string;
  endDay: string;
  source: TagSource;
  reason: string;
  photoCount: number;
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
