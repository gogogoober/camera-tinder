import type { CullerApi } from './CullerApi';
import type { CoreEvent, DaySummary, Decision, Photo, SourceInfo, Stats } from './types';

/**
 * In-memory fake backend so the UI can be built and tested in a plain browser
 * (`npm run dev:mock`), with no Rust build in the loop.
 */

const listeners = new Set<(e: CoreEvent) => void>();
const sources: SourceInfo[] = [];
let photos: Photo[] = [];
const undoStack: [number, Decision | null][] = [];
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function makePhotos(sourceId: string, startId: number): Photo[] {
  const out: Photo[] = [];
  const days = ['2026-07-01', '2026-07-02', '2026-07-03', '2026-07-05'];
  let id = startId;
  days.forEach((day, d) => {
    const count = [72, 140, 45, 98][d];
    for (let i = 0; i < count; i++) {
      const mins = 8 * 60 + i * 4;
      const t = `${day}T${String(Math.floor(mins / 60)).padStart(2, '0')}:${String(mins % 60).padStart(2, '0')}:00`;
      out.push({
        id: id++,
        fingerprint: `mock${id}`,
        sourceId,
        relPath: `DCIM/100MOCK/DSC_${String(id).padStart(4, '0')}.JPG`,
        fileName: `DSC_${String(id).padStart(4, '0')}.JPG`,
        kind: 'jpeg',
        size: 8_000_000,
        capturedAt: t,
        camera: id % 3 === 0 ? 'FUJIFILM X-T5' : 'NIKON Z 6_2',
        decision: null,
      });
    }
  });
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
  async scan() {
    let added = 0;
    for (const s of sources) {
      if (photos.some((p) => p.sourceId === s.id)) continue;
      const fresh = makePhotos(s.id, photos.length + 1);
      for (let done = 0; done <= fresh.length; done += 25) {
        listeners.forEach((cb) => cb({ type: 'scanProgress', sourceId: s.id, done: Math.min(done, fresh.length), total: fresh.length }));
        await sleep(40);
      }
      photos = photos.concat(fresh);
      added += fresh.length;
    }
    return { found: active().length, new: added, skippedUnsupported: 3, failed: [] };
  },

  async queue(filter) {
    return active()
      .filter((p) => filter.day == null || (filter.day === 'unknown' ? p.capturedAt == null : p.capturedAt?.startsWith(filter.day)))
      .filter((p) => {
        switch (filter.status) {
          case 'undecided': return p.decision == null;
          case 'keepers': return p.decision === 'like' || p.decision === 'favorite';
          case 'rejects': return p.decision === 'dislike' || p.decision === 'hate';
          default: return true;
        }
      })
      .map((p) => ({ ...p }));
  },
  async days() {
    const byDay = new Map<string | null, Photo[]>();
    for (const p of active()) {
      const d = p.capturedAt?.slice(0, 10) ?? null;
      byDay.set(d, [...(byDay.get(d) ?? []), p]);
    }
    return [...byDay.entries()].map(([day, list]): DaySummary => ({ day, stats: statsOf(list) }));
  },
  async stats() {
    return statsOf(active());
  },

  async decide(id, decision) {
    const p = photos.find((p) => p.id === id)!;
    undoStack.push([id, p.decision]);
    p.decision = decision;
  },
  async undo() {
    const last = undoStack.pop();
    if (!last) return null;
    const p = photos.find((p) => p.id === last[0])!;
    p.decision = last[1];
    return { ...p };
  },

  async exportKeepers(dest, options) {
    const keep = active().filter((p) => (p.decision === 'like' && options.includeLikes) || (p.decision === 'favorite' && options.includeFavorites));
    for (let i = 1; i <= keep.length; i++) {
      if (i % 10 === 0 || i === keep.length) listeners.forEach((cb) => cb({ type: 'exportProgress', done: i, total: keep.length }));
      await sleep(5);
    }
    return { destination: dest, copied: keep.length, alreadyThere: 0, failed: [] };
  },

  previewUrl(id) {
    const hue = (id * 47) % 360;
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1500" height="1000" viewBox="0 0 1500 1000">
      <defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
        <stop offset="0" stop-color="hsl(${hue},55%,55%)"/><stop offset="1" stop-color="hsl(${(hue + 60) % 360},50%,25%)"/>
      </linearGradient></defs>
      <rect width="1500" height="1000" fill="url(#g)"/>
      <circle cx="${300 + ((id * 97) % 900)}" cy="${250 + ((id * 53) % 400)}" r="160" fill="hsla(${(hue + 180) % 360},70%,75%,.55)"/>
      <text x="60" y="940" font-family="system-ui" font-size="64" fill="white" opacity=".85">mock photo #${id}</text>
    </svg>`;
    return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
  },
  async onEvent(cb) {
    listeners.add(cb);
    return () => listeners.delete(cb);
  },
};
