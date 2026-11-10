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
  day: string | null; // "YYYY-MM-DD", "unknown", or null for all
  status: StatusFilter;
}

export interface ScanReport {
  found: number;
  new: number;
  skippedUnsupported: number;
  failed: [string, string][];
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
  | { type: 'exportProgress'; done: number; total: number };
