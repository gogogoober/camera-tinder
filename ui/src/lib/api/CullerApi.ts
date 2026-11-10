import type {
  CoreEvent,
  DaySummary,
  Decision,
  ExportOptions,
  ExportReport,
  Filter,
  Photo,
  ScanReport,
  SourceInfo,
  Stats,
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
  days(): Promise<DaySummary[]>;
  stats(): Promise<Stats>;

  decide(id: number, decision: Decision): Promise<void>;
  /** Reverts the last decision; returns that photo (now undecided or restored). */
  undo(): Promise<Photo | null>;

  exportKeepers(dest: string, options: ExportOptions): Promise<ExportReport>;

  /** URL for a display-sized preview, usable directly in <img src>. */
  previewUrl(id: number, edge?: number): string;
  /** Subscribe to progress events. Returns an unsubscribe function. */
  onEvent(cb: (e: CoreEvent) => void): Promise<() => void>;
}
