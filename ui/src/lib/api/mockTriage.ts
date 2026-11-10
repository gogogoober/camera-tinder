// Fake triage for the mock backend (npm run dev:mock): deterministic, plausible, not real.
// Real routing lives in cull-core/src/triage.

import type { IssueKind } from './analysisTypes';
import { mockSharpness } from './mockAnalysis';
import type { Group, Photo, TriageItem } from './types';

const SWEEP_REASONS: IssueKind[] = ['blurry', 'underexposed', 'overexposed', 'accidental', 'motionBlur', 'faceCutOff'];
const HINTS: IssueKind[] = ['tilted', 'colorCast', 'noisy', 'eyesClosed', 'lowAesthetic'];

const base = (p: Photo): TriageItem => ({ photoId: p.id, question: 'judge', group: null, reasons: [], hints: [], score: null, analyzed: true });

/** One item per photo of `list`; `bursts` (frames seconds apart) become Pick groups. */
export function mockTriage(list: Photo[], bursts: Group[]): TriageItem[] {
  const items = list.map(base);

  bursts.forEach((g, b) => {
    const frames = list.slice(g.start, g.end);
    const allBad = b % 4 === 0; // every fourth burst has no good frame
    const rank = frames.map((p, k) => ({ k, sharp: allBad ? 0 : mockSharpness(p.id) })).sort((x, y) => y.sharp - x.sharp);
    frames.forEach((p, k) => {
      const r = rank.findIndex((x) => x.k === k);
      const bad = allBad || mockSharpness(p.id) < 25;
      items[g.start + k] = {
        ...base(p),
        question: 'pick',
        reasons: bad ? ['blurry'] : [],
        group: { id: frames[0].id, kind: 'burst', size: frames.length, rank: r, suggested: r === 0 && !bad },
      };
    });
  });

  list.forEach((p, i) => {
    if (items[i].group) return;
    if (mockSharpness(p.id) < 20 || p.id % 11 === 0) {
      items[i] = { ...items[i], question: 'sweep', reasons: [SWEEP_REASONS[p.id % SWEEP_REASONS.length]] };
    } else if (p.id % 6 === 0) {
      items[i] = { ...items[i], hints: [HINTS[p.id % HINTS.length]] };
    }
  });
  return items;
}
