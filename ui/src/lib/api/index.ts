import type { CullerApi } from './CullerApi';
import { mockApi } from './mockApi';
import { tauriApi } from './tauriApi';

export type { CullerApi } from './CullerApi';
export * from './types';

const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

/** The single backend the UI talks to: real Tauri when available, mock otherwise. */
export const api: CullerApi = inTauri && import.meta.env.MODE !== 'mock' ? tauriApi : mockApi;
export const isMock = api === mockApi;
