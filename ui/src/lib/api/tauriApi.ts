import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import type { CullerApi } from './CullerApi';
import type { CoreEvent } from './types';

async function pickDirectory(title: string): Promise<string | null> {
  const picked = await open({ directory: true, multiple: false, title });
  return typeof picked === 'string' ? picked : null;
}

export const tauriApi: CullerApi = {
  pickSourceFolder: () => pickDirectory('Choose a folder or SD card to cull'),
  pickExportFolder: () => pickDirectory('Choose where to copy your keepers'),

  addFolder: (path) => invoke('add_folder', { path }),
  removeSource: (id) => invoke('remove_source', { id }),
  forgetSource: (id) => invoke('forget_source', { id }),
  library: () => invoke('library'),
  listSources: () => invoke('list_sources'),
  scan: (request) => invoke('scan', { request: request ?? null }),
  cancelScan: () => invoke('cancel_scan'),

  queue: (filter) => invoke('queue', { filter }),
  plan: (filter) => invoke('plan', { filter }),
  days: () => invoke('days'),
  stats: () => invoke('stats'),

  decide: (id, decision) => invoke('decide', { id, decision }),
  logOutcomes: (entries) => invoke('log_outcomes', { entries }),
  resetDays: (request) => invoke('reset_days', { request }),
  decideMany: (ids, decision) => invoke('decide_many', { ids, decision }),
  undo: () => invoke('undo'),

  suggestTags: (dismissed) => invoke('suggest_tags', { dismissed: dismissed ?? [] }),
  calendarStatus: () => invoke('calendar_status'),
  connectCalendar: () => invoke('connect_calendar'),
  previewCacheStatus: () => invoke('preview_cache_status'),
  clearPreviews: () => invoke('clear_previews'),

  exportKeepers: (dest, options) => invoke('export_keepers', { dest, options }),
  exportCount: (options) => invoke('export_count', { options }),
  listTags: () => invoke('list_tags'),
  saveTag: (name, days) => invoke('save_tag', { name, days }),
  deleteTag: (id) => invoke('delete_tag', { id }),

  analyzerInfo: () => invoke('analyzer_info'),
  analyzePhotos: (request = {}) => invoke('analyze_photos', { request }),
  stopAnalysis: () => invoke('stop_analysis'),
  resumeAnalysis: () => invoke('resume_analysis'),
  analysisStopped: () => invoke('analysis_stopped'),
  errorLog: (maxLines) => invoke('error_log', { maxLines }),
  photoAnalysis: (id) => invoke('photo_analysis', { id }),
  photoAnalyses: (filter) => invoke('photo_analyses', { filter }),
  photoIssues: (filter) => invoke('photo_issues', { filter }),
  similarGroups: (filter, options) => invoke('similar_groups', { filter, options: options ?? null }),

  // preview://localhost/<id> on macOS/iOS, http://preview.localhost/<id> on Windows/Android.
  previewUrl: (id, edge = 2048) => `${convertFileSrc(String(id), 'preview')}?edge=${edge}`,
  onEvent: (cb) => listen<CoreEvent>('core-event', (e) => cb(e.payload)),
};
