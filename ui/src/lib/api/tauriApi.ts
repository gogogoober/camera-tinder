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
  listSources: () => invoke('list_sources'),
  scan: () => invoke('scan'),

  queue: (filter) => invoke('queue', { filter }),
  days: () => invoke('days'),
  stats: () => invoke('stats'),

  decide: (id, decision) => invoke('decide', { id, decision }),
  undo: () => invoke('undo'),

  exportKeepers: (dest, options) => invoke('export_keepers', { dest, options }),

  // preview://localhost/<id> on macOS/iOS, http://preview.localhost/<id> on Windows/Android.
  previewUrl: (id, edge = 2048) => `${convertFileSrc(String(id), 'preview')}?edge=${edge}`,
  onEvent: (cb) => listen<CoreEvent>('core-event', (e) => cb(e.payload)),
};
