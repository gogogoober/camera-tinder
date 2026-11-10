import type { CullerApi } from './CullerApi';
import type { CalendarStatus, CoreEvent, DaySummary, Decision, Filter, Group, Photo, Plan, SourceInfo, Stats, Tag, TagSuggestion } from './types';
import type { PhotoAnalysis } from './analysisTypes';
import { mockTriage } from './mockTriage';
import { MOCK_ANALYZERS, mockAnalysis, mockGroups, mockIssues, mockSharpness } from './mockAnalysis';

/**
 * In-memory fake backend so the UI can be built and tested in a plain browser
 * (`npm run dev:mock`), with no Rust build in the loop. It mimics cull-core:
 * capture-ordered queues, moments/bursts, tags and suggestions.
 */

/** Shape of a photo nobody has analysed yet. */
const EMPTY = {
  blur: null, exposure: null, contrast: null, noise: null, colorCast: null, accidental: null, perceptualHash: null,
  similarity: null, faces: null, eyes: null, faceFraming: null, faceQuality: null, saliency: null, subjectFocus: null,
  aesthetics: null, horizon: null, errors: {},
};

const listeners = new Set<(e: CoreEvent) => void>();
const sources: SourceInfo[] = [];
let photos: Photo[] = [];
let cancelRequested = false;
/** Folders whose photos are all saved, and the photos a stopped scan hasn't saved yet. */
const scannedSources = new Set<string>();
const unsaved = new Map<string, Photo[]>();
let tags: Tag[] = [
  { id: 1, name: 'Lisbon', startDay: '2026-06-12', endDay: '2026-06-19', source: 'place' },
];
let tagId = 2;
let calendar: CalendarStatus = 'notDetermined';
let cacheBytes = 0;
const undoStack: [number, Decision | null][][] = [];
/** Photos "analysed" so far (results are generated deterministically from the id). */
const analysed = new Map<number, PhotoAnalysis>();
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// Background analysis, like the real worker: runs as photos are scanned, in chunks, until stopped.
let analysisStopped = false;
let analysisRunning = false;
async function backgroundAnalysis() {
  if (analysisRunning) return;
  analysisRunning = true;
  try {
    while (!analysisStopped) {
      const todo = photos.filter((p) => !analysed.has(p.id));
      if (!todo.length) break;
      const total = analysed.size + todo.length;
      for (let i = 0; i < todo.length && !analysisStopped; i += 100) {
        for (const p of todo.slice(i, i + 100)) analysed.set(p.id, mockAnalysis(p));
        listeners.forEach((cb) => cb({ type: 'analysisProgress', done: analysed.size, total }));
        await sleep(80);
      }
    }
  } finally {
    analysisRunning = false;
  }
}

function rng(seed: number) {
  let s = seed >>> 0 || 1;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 4294967296;
  };
}

const pad = (n: number) => String(n).padStart(2, '0');
const dayStr = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;

/** Trips with a base number of photos per day. Everything else is a light scatter. */
const TRIPS: [string, string, number][] = [
  ['2026-06-12', '2026-06-19', 140],
  ['2026-08-01', '2026-08-03', 150],
  ['2026-09-19', '2026-09-19', 300],
  ['2026-10-17', '2026-10-18', 120],
  ['2026-11-01', '2026-11-08', 60],
];

function makePhotos(sourceId: string, startId: number): Photo[] {
  const out: Photo[] = [];
  let id = startId;
  const first = new Date(2026, 4, 17);
  for (let i = 0; i < 175; i++) {
    const d = new Date(first.getFullYear(), first.getMonth(), first.getDate() + i);
    const day = dayStr(d);
    const r = rng(i * 31 + 5);
    const trip = TRIPS.find(([a, b]) => day >= a && day <= b);
    let count = 0;
    if (trip) count = Math.round(trip[2] * (0.4 + r() * 1.1));
    else {
      const v = r();
      count = v < 0.55 ? 0 : v < 0.9 ? 3 + Math.floor(r() * 22) : 30 + Math.floor(r() * 50);
    }
    // Moments of a few photos, spread over the day, some with bursts.
    let t = 7 * 3600 + Math.floor(r() * 3600);
    let made = 0;
    while (made < count) {
      const size = Math.min(count - made, 3 + Math.floor(r() * 20));
      for (let k = 0; k < size; ) {
        const burst = size - k >= 2 && r() < 0.16 ? Math.min(2 + Math.floor(r() * 4), size - k) : 1;
        for (let j = 0; j < burst; j++, k++) {
          const sec = Math.min(t, 23 * 3600 + 59 * 60);
          out.push({
            id,
            fingerprint: `mock${id}`,
            sourceId,
            relPath: `DCIM/100MOCK/DSC_${String(id).padStart(4, '0')}.JPG`,
            fileName: `DSC_${String(id).padStart(4, '0')}.JPG`,
            kind: 'jpeg',
            size: 8_000_000,
            capturedAt: `${day}T${pad(Math.floor(sec / 3600))}:${pad(Math.floor(sec / 60) % 60)}:${pad(sec % 60)}`,
            camera: id % 3 === 0 ? 'FUJIFILM X-T5' : 'NIKON Z f',
            decision: null,
          });
          id++;
          t += burst > 1 ? 1 : 4 + Math.floor(r() * 45);
        }
        if (burst > 1) t += 5 + Math.floor(r() * 30);
      }
      made += size;
      t += 15 * 60 + Math.floor(r() * 3600);
    }
  }
  // Pre-cull everything before August and half the wedding, like a real library.
  for (const p of out) {
    const day = p.capturedAt!.slice(0, 10);
    if (day < '2026-08-01' || (day >= '2026-08-04' && day < '2026-09-19')) p.decision = 'like';
  }
  return out;
}

function statsOf(list: Photo[]): Stats {
  const s: Stats = { total: list.length, undecided: 0, like: 0, dislike: 0, favorite: 0, hate: 0 };
  for (const p of list) {
    if (p.decision) s[p.decision]++;
    else s.undecided++;
  }
  return s;
}

const active = () => photos.filter((p) => sources.some((s) => s.id === p.sourceId));

function query(filter: Filter): Photo[] {
  return active()
    .filter((p) => {
      if (filter.day == null) return true;
      if (filter.day === 'unknown') return p.capturedAt == null;
      const d = p.capturedAt?.slice(0, 10);
      if (!d) return false;
      return filter.dayTo ? d >= filter.day && d <= filter.dayTo : d === filter.day;
    })
    .filter((p) => {
      switch (filter.status) {
        case 'undecided': return p.decision == null;
        case 'keepers': return p.decision === 'like' || p.decision === 'favorite';
        case 'rejects': return p.decision === 'dislike' || p.decision === 'hate';
        default: return true;
      }
    })
    .sort((a, b) => (a.capturedAt ?? '9').localeCompare(b.capturedAt ?? '9'))
    .map((p) => ({ ...p }));
}

function groupByGap(list: Photo[], gapSecs: number, minSize = 1): Group[] {
  const ms = (p: Photo) => (p.capturedAt ? new Date(p.capturedAt).getTime() : NaN);
  const out: Group[] = [];
  let start = 0;
  for (let i = 1; i <= list.length; i++) {
    const split = i === list.length || Math.abs(ms(list[i]) - ms(list[i - 1])) > gapSecs * 1000;
    if (split) {
      if (i - start >= minSize) out.push({ start, end: i });
      start = i;
    }
  }
  return out;
}

export const mockApi: CullerApi = {
  async pickSourceFolder() {
    return `/Volumes/MOCK_CARD_${sources.length + 1}`;
  },
  async pickExportFolder() {
    return '/Users/you/Pictures/Keepers';
  },

  async addFolder(path) {
    const info = { id: path, name: path.split('/').pop() ?? path, photoCount: 0 };
    sources.push(info);
    return info;
  },
  async removeSource(id) {
    const i = sources.findIndex((s) => s.id === id);
    if (i >= 0) sources.splice(i, 1);
  },
  async listSources() {
    return sources.map((s) => ({ ...s, photoCount: photos.filter((p) => p.sourceId === s.id).length }));
  },
  async scan(request = {}) {
    cancelRequested = false;
    let added = 0;
    let cancelled = false;
    const emit = (e: CoreEvent) => listeners.forEach((cb) => cb(e));
    for (const s of sources) {
      if (request.sourceIds && !request.sourceIds.includes(s.id)) continue;
      if (cancelRequested) {
        cancelled = true; // Stop also skips the folders not reached yet
        break;
      }
      // Like the real backend: photos already indexed aren't read again (the mock has no new files to find).
      if (scannedSources.has(s.id)) continue;
      const fresh = unsaved.get(s.id) ?? makePhotos(s.id, photos.length + 1);
      // Saved in batches, so the Pick screen fills in while the scan runs.
      let savedHere = 0;
      for (let done = 0; done < fresh.length && !cancelled; done += 120) {
        await sleep(120);
        const batch = fresh.slice(done, done + 120);
        photos = photos.concat(batch);
        added += batch.length;
        savedHere += batch.length;
        emit({ type: 'scanProgress', sourceId: s.id, done: done + batch.length, total: fresh.length });
        emit({ type: 'scanSaved', sourceId: s.id, saved: added });
        void backgroundAnalysis();
        cancelled = cancelRequested;
      }
      if (savedHere < fresh.length) {
        unsaved.set(s.id, fresh.slice(savedHere));
        break;
      }
      cancelled = false; // stopped after the last batch: nothing was left out
      unsaved.delete(s.id);
      scannedSources.add(s.id);
    }
    return { found: active().length, new: added, skippedUnsupported: 3, otherFiles: 2, failed: [], cancelled };
  },
  async cancelScan() {
    cancelRequested = true;
  },

  async queue(filter) {
    return query(filter);
  },
  async plan(filter): Promise<Plan> {
    const list = query(filter);
    const bursts = groupByGap(list, 3, 2);
    return { photos: list, moments: groupByGap(list, 15 * 60), bursts, triage: mockTriage(list, bursts), triageVersion: 'mock' };
  },
  async days() {
    const byDay = new Map<string | null, Photo[]>();
    for (const p of active()) {
      const d = p.capturedAt?.slice(0, 10) ?? null;
      byDay.set(d, [...(byDay.get(d) ?? []), p]);
    }
    return [...byDay.entries()].map(([day, list]): DaySummary => ({ day, stats: statsOf(list) })).sort((a, b) => (a.day ?? '9').localeCompare(b.day ?? '9'));
  },
  async stats() {
    return statsOf(active());
  },

  async decide(id, decision) {
    return this.decideMany([id], decision);
  },
  async decideMany(ids, decision) {
    const batch: [number, Decision | null][] = [];
    for (const id of ids) {
      const p = photos.find((p) => p.id === id);
      if (!p) continue;
      batch.push([id, p.decision]);
      p.decision = decision;
    }
    if (batch.length) undoStack.push(batch);
  },
  async undo() {
    const batch = undoStack.pop();
    if (!batch) return [];
    return batch.map(([id, prev]) => {
      const p = photos.find((p) => p.id === id)!;
      p.decision = prev;
      return { ...p };
    });
  },

  async tags() {
    return tags.map((t) => ({ ...t }));
  },
  async addTag(name, startDay, endDay, source = 'manual') {
    const t: Tag = { id: tagId++, name, startDay, endDay, source };
    tags.push(t);
    return t;
  },
  async removeTag(id) {
    tags = tags.filter((t) => t.id !== id);
  },
  async suggestTags(): Promise<TagSuggestion[]> {
    const sug = (name: string, startDay: string, endDay: string, source: 'gap' | 'calendar', reason: string, children: TagSuggestion[] = [], names: string[] = []): TagSuggestion =>
      ({ name, startDay, endDay, source, reason, photoCount: 0, names, children });
    const all: TagSuggestion[] = [
      sug('Hike', '2026-08-01', '2026-08-03', 'gap', 'Shot 3 days in a row'),
      sug("Maya's wedding", '2026-09-19', '2026-09-19', 'calendar', 'Calendar event, 2 pm – 11 pm'),
      sug('Cabin weekend', '2026-10-17', '2026-10-18', 'calendar', 'All-day calendar event'),
      sug('Mom Visiting', '2026-10-29', '2026-11-08', 'calendar', 'Calendar event', [
        sug('Wedding – Washington DC / Mayflower Hotel Stay', '2026-11-05', '2026-11-08', 'calendar', 'Calendar event', [], ['Wedding – Washington DC', 'Mayflower Hotel Stay']),
        sug('Train to Washington DC', '2026-11-05', '2026-11-05', 'calendar', 'Calendar event'),
        sug('Mariel Hangout', '2026-11-08', '2026-11-08', 'calendar', 'Calendar event'),
      ], ['Mom Visiting']),
      sug('Nov 1 – Nov 8', '2026-11-01', '2026-11-08', 'gap', 'Shot 8 days in a row'),
    ];
    const taken = (s: TagSuggestion) => tags.some((t) => t.startDay === s.startDay && t.endDay === s.endDay);
    return all
      .filter((s) => calendar === 'connected' || s.source !== 'calendar')
      .filter((s) => !taken(s))
      .map((s) => ({ ...s, children: s.children.filter((c) => !taken(c)) }));
  },

  async calendarStatus() {
    return calendar;
  },
  async connectCalendar() {
    await sleep(400);
    calendar = 'connected';
    return calendar;
  },

  async previewCacheStatus() {
    cacheBytes ||= active().length * 520_000;
    // Mock: a source counts as connected while it's in the session (remove it with ✕ to "unplug").
    return { files: active().length * 2, bytes: cacheBytes, connectedSources: sources.map((s) => s.name) };
  },
  async clearPreviews() {
    if (sources.length) throw new Error(`disconnect these first: ${sources.map((s) => s.name).join(', ')}`);
    const freed = cacheBytes;
    cacheBytes = 0;
    return freed;
  },

  async exportKeepers(dest, options) {
    const keep = active().filter((p) => (p.decision === 'like' && options.includeLikes) || (p.decision === 'favorite' && options.includeFavorites));
    for (let i = 1; i <= keep.length; i += 50) {
      listeners.forEach((cb) => cb({ type: 'exportProgress', done: Math.min(i, keep.length), total: keep.length }));
      await sleep(5);
    }
    return { destination: dest, copied: keep.length, alreadyThere: 0, failed: [] };
  },

  async stopAnalysis() {
    analysisStopped = true;
  },
  async resumeAnalysis() {
    analysisStopped = false;
    void backgroundAnalysis();
  },
  async analysisStopped() {
    return analysisStopped;
  },
  async logOutcomes() {},
  async analyzerInfo() {
    return MOCK_ANALYZERS;
  },
  async analyzePhotos(request = {}) {
    const list = request.photoIds
      ? photos.filter((p) => request.photoIds!.includes(p.id))
      : query(request.filter ?? { day: null, status: 'all' });
    const todo = request.force ? list : list.filter((p) => !analysed.has(p.id));
    for (let i = 0; i < todo.length; i++) {
      analysed.set(todo[i].id, mockAnalysis(todo[i]));
      if (i % 200 === 0 || i === todo.length - 1) {
        listeners.forEach((cb) => cb({ type: 'analysisProgress', done: i + 1, total: todo.length }));
        await sleep(20);
      }
    }
    return { photos: list.length, analyzed: todo.length, upToDate: list.length - todo.length, kindErrors: 0, failed: [] };
  },
  async photoAnalysis(id) {
    return analysed.get(id) ?? { ...mockAnalysis(photos.find((p) => p.id === id)!), implementations: {}, ...EMPTY };
  },
  async photoAnalyses(filter) {
    return query(filter).flatMap((p) => (analysed.has(p.id) ? [analysed.get(p.id)!] : []));
  },
  async photoIssues(filter) {
    return (await this.photoAnalyses(filter)).map(mockIssues).filter((i) => i.issues.length > 0);
  },
  async similarGroups(filter) {
    const list = query(filter).filter((p) => analysed.has(p.id));
    return mockGroups(list, groupByGap(list, 3, 2), (p) => analysed.get(p.id)!);
  },

  previewUrl(id) {
    const hue = (id * 47) % 360;
    const soft = mockSharpness(id) < 22; // blurry in the picture too, matching the mock blur analysis
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1500" height="1000" viewBox="0 0 1500 1000">
      <defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
        <stop offset="0" stop-color="hsl(${hue},55%,55%)"/><stop offset="1" stop-color="hsl(${(hue + 60) % 360},50%,25%)"/>
      </linearGradient>${soft ? '<filter id="b"><feGaussianBlur stdDeviation="14"/></filter>' : ''}</defs>
      <g ${soft ? 'filter="url(#b)"' : ''}>
        <rect width="1500" height="1000" fill="url(#g)"/>
        <circle cx="${300 + ((id * 97) % 900)}" cy="${250 + ((id * 53) % 400)}" r="160" fill="hsla(${(hue + 180) % 360},70%,75%,.55)"/>
        <path d="M0 760 L260 560 L520 720 L820 500 L1180 720 L1500 600 V1000 H0Z" fill="hsla(${hue},30%,12%,.8)"/>
      </g>
      <text x="60" y="940" font-family="system-ui" font-size="64" fill="white" opacity=".85">mock photo #${id}</text>
    </svg>`;
    return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
  },
  async onEvent(cb) {
    listeners.add(cb);
    return () => listeners.delete(cb);
  },
};
