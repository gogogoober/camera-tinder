// Browser mock of the offload flow (card → backup drive): a fixed card, fake progress, honours Stop.
import type { CullerApi } from './CullerApi';
import type { CoreEvent, FolderChoice, OffloadFolder, OffloadPlan, OffloadReport } from './types';

type Emit = (e: CoreEvent) => void;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const MB = 1 << 20;

interface MockFile {
  path: string;
  brand: string;
  day: string;
  kind: 'jpeg' | 'raw' | 'heif' | 'video';
  bytes: number;
  trip: string | null;
}

/** 2026-10-02 … 2026-10-14 is "Japan trip" (a saved tag); "Kyoto" is narrower; November is untagged. */
function card(): MockFile[] {
  const out: MockFile[] = [];
  const add = (brand: string, day: string, n: number, trip: string | null) => {
    for (let i = 0; i < n; i++) {
      const stem = brand === 'Fuji' ? `DSCF${String(out.length + 1).padStart(4, '0')}` : `DSC_${String(out.length + 1).padStart(4, '0')}`;
      const dir = brand === 'Fuji' ? 'DCIM/100_FUJI' : 'DCIM/100NZ6_2';
      out.push({ path: `${dir}/${stem}.JPG`, brand, day, kind: 'jpeg', bytes: 12 * MB, trip });
      out.push({ path: `${dir}/${stem}.${brand === 'Fuji' ? 'RAF' : 'NEF'}`, brand, day, kind: 'raw', bytes: 45 * MB, trip });
      if (i % 9 === 0) out.push({ path: `${dir}/${stem}.MOV`, brand, day, kind: 'video', bytes: 600 * MB, trip });
    }
  };
  add('Fuji', '2026-10-03', 40, 'Japan trip');
  add('Fuji', '2026-10-06', 60, 'Japan trip');
  add('Nikon', '2026-10-09', 30, 'Japan trip');
  add('Fuji', '2026-11-02', 25, null);
  add('Fuji', '2026-11-04', 10, null);
  return out;
}

const ALREADY = 64; // the first files of the card are on the drive already

export function mockOffload(emit: Emit): Pick<CullerApi, 'pickOffloadCard' | 'pickOffloadLibrary' | 'offloadPlan' | 'offloadLayout' | 'offloadStart' | 'offloadStop'> {
  let files: MockFile[] | null = null;
  let library = '';
  let cardRoot = '';
  let fullCheck = false;
  let stop = false;
  const backedUp = new Set<string>();

  const layout = (choices: FolderChoice[]): { plan: OffloadPlan; dest: Map<string, string> } => {
    const all = files ?? [];
    const fresh = all.filter((f, i) => i >= ALREADY && !backedUp.has(f.path));
    const groups = new Map<string, { f: OffloadFolder; days: Set<string> }>();
    const dest = new Map<string, string>();
    for (const f of fresh) {
      const year = Number(f.day.slice(0, 4));
      const tripId = f.trip ? `${f.brand}|${year}|trip:${f.trip.toLowerCase()}` : null;
      const split = tripId && choices.find((c) => c.id === tripId)?.byMonth;
      const isTrip = !!f.trip && !split;
      const key = isTrip ? `trip:${f.trip!.toLowerCase()}` : `month:${f.day.slice(0, 7)}`;
      const id = `${f.brand}|${year}|${key}`;
      const typed = choices.find((c) => c.id === id)?.name?.trim();
      const name = typed || (isTrip ? f.trip! : f.day.slice(0, 7));
      let g = groups.get(id);
      if (!g) {
        g = {
          days: new Set(),
          f: {
            id, brand: f.brand, year, name, path: `Photos/${f.brand}/${year}/${name}`, isTrip, exists: f.brand === 'Fuji' && isTrip,
            files: 0, bytes: 0, jpeg: 0, raw: 0, heif: 0, video: 0, firstDay: null, lastDay: null,
            options: isTrip ? ['Kyoto'].filter((n) => n !== name) : [], splitFrom: [],
          },
        };
        groups.set(id, g);
      }
      if (split && tripId && !g.f.splitFrom.some(([i]) => i === tripId)) g.f.splitFrom.push([tripId, f.trip!]);
      g.days.add(f.day);
      g.f.files++;
      g.f.bytes += f.bytes;
      g.f[f.kind]++;
      const sub = { jpeg: '', raw: 'RAW/', heif: 'HIF/', video: 'Videos/' }[f.kind];
      dest.set(f.path, `${g.f.path}/${sub}${f.path.split('/').pop()}`);
    }
    const folders = [...groups.values()].map(({ f, days }) => {
      const d = [...days].sort();
      return { ...f, firstDay: d[0] ?? null, lastDay: d[d.length - 1] ?? null };
    });
    const already = all.filter((f, i) => i < ALREADY || backedUp.has(f.path));
    const newBytes = fresh.reduce((s, f) => s + f.bytes, 0);
    return {
      dest,
      plan: {
        cardRoot, libraryRoot: library,
        newFiles: fresh.length, newBytes,
        alreadyFiles: already.length, alreadyBytes: already.reduce((s, f) => s + f.bytes, 0),
        alreadyIn: already.length
          ? [
              { folder: 'Photos/Fuji/2026/Japan trip', files: Math.max(0, already.length - 24) },
              { folder: 'Old backups/2026 japan (copy)', files: Math.min(24, already.length) },
            ].filter((x) => x.files > 0)
          : [],
        skipped: [{ extension: 'LRF', count: 7 }, { extension: 'THM', count: 3 }],
        unreadable: [],
        folders,
        freeBytes: 1.6e12,
        fsType: library.includes('EXFAT') ? 'exfat' : 'apfs',
        warnings: library.includes('EXFAT')
          ? ['This drive is formatted EXFAT. It has no journal, so a power cut or unplugging it mid-copy can damage its folders. APFS is safer for a backup drive.']
          : [],
        lastBackups: [
          { brand: 'Fuji', lastDay: '2026-09-28', at: '2026-09-30T21:14:00' },
          { brand: 'Nikon', lastDay: '2026-08-17', at: '2026-08-20T19:02:00' },
        ],
        fullCheck,
      },
    };
  };

  return {
    async pickOffloadCard() {
      return '/Volumes/NIKON Z6II';
    },
    async pickOffloadLibrary() {
      return '/Volumes/T7 Shield';
    },
    async offloadPlan(c, lib, full) {
      stop = false;
      cardRoot = c;
      library = lib;
      fullCheck = full;
      files = card();
      for (let i = 0; i <= files.length; i += 40) {
        if (stop) throw new Error('stopped');
        emit({ type: 'offloadProgress', phase: 'card', done: Math.min(i, files.length), total: files.length, bytesDone: 0, bytesTotal: 0, current: files[Math.min(i, files.length - 1)].path });
        await sleep(30);
      }
      emit({ type: 'offloadProgress', phase: 'library', done: 0, total: 1, bytesDone: 0, bytesTotal: 0, current: '' });
      await sleep(200);
      return layout([]).plan;
    },
    async offloadLayout(choices) {
      if (!files) throw new Error('no backup plan yet');
      return layout(choices).plan;
    },
    async offloadStart(choices, readCardTwice): Promise<OffloadReport> {
      if (!files) throw new Error('no backup plan yet');
      stop = false;
      const { plan, dest } = layout(choices);
      const todo = files.filter((f) => dest.has(f.path));
      const started = Date.now();
      let bytes = 0;
      let done = 0;
      const speed = 180 * MB; // bytes per simulated second; one simulated second = 40 ms
      for (const f of todo) {
        if (stop) break;
        for (let b = 0; b < f.bytes; b += speed / 4) {
          if (stop) break;
          bytes += Math.min(speed / 4, f.bytes - b);
          emit({ type: 'offloadProgress', phase: 'copy', done, total: todo.length, bytesDone: bytes, bytesTotal: plan.newBytes, current: f.path });
          await sleep(readCardTwice ? 20 : 10);
        }
        if (stop) break;
        done++;
        backedUp.add(f.path);
      }
      emit({ type: 'offloadProgress', phase: 'copy', done, total: todo.length, bytesDone: bytes, bytesTotal: plan.newBytes, current: '' });
      const stopped = stop;
      const notBackedUp = files.filter((f, i) => i >= ALREADY && !backedUp.has(f.path)).map((f) => f.path);
      files = null;
      return {
        libraryRoot: library,
        copied: done,
        bytes: todo.slice(0, done).reduce((s, f) => s + f.bytes, 0),
        already: plan.alreadyFiles,
        failed: [],
        notBackedUp,
        recheckError: null,
        skipped: plan.skipped,
        stopped,
        error: null,
        manifest: done ? 'Photos/_darkroom/offload-2026-11-10-153000.txt' : null,
        seconds: (Date.now() - started) / 1000,
        allSafe: !stopped && notBackedUp.length === 0,
        cardReadTwice: readCardTwice,
        stages: {
          readBytes: bytes * (readCardTwice ? 2 : 1), readSeconds: (bytes * (readCardTwice ? 2 : 1)) / (180 * MB),
          writeBytes: bytes, writeSeconds: bytes / (850 * MB),
          checkBytes: bytes, checkSeconds: bytes / (1000 * MB),
        },
      };
    },
    async offloadStop() {
      stop = true;
    },
  };
}
