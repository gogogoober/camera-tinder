// Deterministic fake photo analysis for the mock backend (npm run dev:mock).
// Shapes match analysisTypes.ts; numbers are plausible, not real.

import type { AnalyzerInfo, Issue, PhotoAnalysis, PhotoIssues, SimilarGroup } from './analysisTypes';
import type { Group, Photo } from './types';

function rng(seed: number) {
  let s = seed >>> 0 || 1;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 4294967296;
  };
}

export const MOCK_ANALYZERS: AnalyzerInfo[] = [
  ['blur', 'rust.blur.v1'],
  ['exposure', 'rust.exposure.v1'],
  ['contrast', 'rust.contrast.v1'],
  ['noise', 'rust.noise.v1'],
  ['colorCast', 'rust.color_cast.v2'],
  ['accidental', 'rust.accidental.v1'],
  ['perceptualHash', 'rust.dhash.v1'],
  ['similarity', 'mock.similarity'],
  ['faces', 'mock.faces'],
  ['eyes', 'rust.eye_aspect_ratio.v1'],
  ['faceFraming', 'rust.face_framing.v1'],
  ['faceQuality', 'mock.face_quality'],
  ['saliency', 'mock.saliency'],
  ['subjectFocus', 'rust.subject_focus.v2'],
  ['aesthetics', 'mock.aesthetics'],
  ['horizon', 'mock.horizon'],
].map(([kind, implementation]) => ({
  kind,
  implementation,
  inputs: kind === 'eyes' || kind === 'faceFraming' ? ['faces'] : kind === 'subjectFocus' ? ['faces', 'saliency'] : [],
})) as AnalyzerInfo[];

/** Whole-frame sharpness 0–100 the mock pretends each photo has (about 1 in 8 is blurry). */
export function mockSharpness(id: number): number {
  const r = rng(id * 104729 + 7);
  return r() < 0.12 ? Math.round(4 + r() * 16) : Math.round(30 + r() * 70);
}

export function mockAnalysis(p: Photo): PhotoAnalysis {
  const r = rng(p.id * 7919 + 13);
  const sharp = mockSharpness(p.id);
  const peak = Math.min(100, sharp + Math.round(r() * 25));
  const mean = 0.15 + r() * 0.7;
  const hasFaces = r() < 0.35;
  const faceCount = hasFaces ? 1 + Math.floor(r() * 3) : 0;
  const faces = Array.from({ length: faceCount }, (_, i) => {
    const x = 0.1 + i * 0.28 + r() * 0.05;
    const cut = r() < 0.08;
    return {
      bounds: { x: cut ? 0 : x, y: 0.25 + r() * 0.2, width: 0.14, height: 0.2 },
      confidence: 0.9,
      roll: 0,
      yaw: 0,
      pitch: 0,
      leftEye: [],
      rightEye: [],
    };
  });
  const eyes = faces.map((_, i) => {
    const closed = r() < 0.12;
    const o = closed ? 0.08 : 0.3;
    return { face: i, leftOpenness: o, rightOpenness: o, closed };
  });
  const tilt = r() < 0.1 ? (r() < 0.5 ? -1 : 1) * (2 + r() * 6) : 0;
  const all = MOCK_ANALYZERS.map((a) => [a.kind, a.implementation!]);
  return {
    photoId: p.id,
    blur: { score: sharp, peak, directionality: r() * 0.6, verdict: peak < 35 ? (r() < 0.4 ? 'motionBlur' : 'soft') : 'sharp' },
    exposure: {
      mean,
      shadowsClipped: mean < 0.2 ? 0.4 : 0.02,
      highlightsClipped: mean > 0.8 ? 0.2 : 0.01,
      p05: mean * 0.3,
      p95: Math.min(1, mean * 1.4),
      verdict: mean < 0.2 ? 'underexposed' : mean > 0.8 ? 'overexposed' : 'ok',
    },
    contrast: { rms: 0.05 + r() * 0.2, range: 0.3 + r() * 0.6, flat: r() < 0.05 },
    noise: { sigma: r() * 3, level: r() < 0.06 ? 'high' : 'low' },
    colorCast: { warmCool: 0, greenMagenta: 0, strength: 0.1, hue: 'neutral' },
    accidental: { likely: r() < 0.01, reason: null },
    perceptualHash: (p.id * 2654435761).toString(16).padStart(16, '0').slice(-16),
    similarity: null,
    faces,
    eyes,
    faceFraming: { cutFaces: faces.flatMap((f, i) => (f.bounds.x === 0 ? [i] : [])) },
    faceQuality: faces.map((f) => ({ bounds: f.bounds, quality: 0.2 + r() * 0.7 })),
    saliency: [{ bounds: { x: 0.3, y: 0.3, width: 0.4, height: 0.4 }, confidence: 0.8 }],
    subjectFocus: { subject: peak, frame: sharp, verdict: peak < 35 ? 'subjectSoft' : 'inFocus' },
    aesthetics: { overall: -0.6 + r() * 1.5, isUtility: r() < 0.02 },
    horizon: { detected: tilt !== 0, angleDegrees: tilt, confidence: 1 },
    implementations: Object.fromEntries(all),
    errors: {},
  };
}

/** Mirrors analysis/issues.rs closely enough for UI work. */
export function mockIssues(a: PhotoAnalysis): PhotoIssues {
  const out: Issue[] = [];
  const add = (kind: Issue['kind'], severity: number, detail: string) => out.push({ kind, severity, detail });
  if (a.blur?.verdict === 'soft') add('blurry', 0.7, `Nothing sharp (peak ${a.blur.peak} / 100)`);
  if (a.blur?.verdict === 'motionBlur') add('motionBlur', 0.7, `Motion blur (peak ${a.blur.peak} / 100)`);
  if (a.subjectFocus?.verdict === 'subjectSoft') add('subjectOutOfFocus', 0.5, 'Subject is soft');
  if (a.exposure?.verdict === 'underexposed') add('underexposed', 0.5, `Dark (mean ${Math.round(a.exposure.mean * 100)}%)`);
  if (a.exposure?.verdict === 'overexposed') add('overexposed', 0.5, 'Blown highlights');
  if (a.contrast?.flat) add('flat', 0.3, 'Flat / hazy');
  if (a.noise?.level === 'high') add('noisy', 0.5, 'Very grainy');
  const closed = a.eyes?.filter((e) => e.closed).length ?? 0;
  if (closed) add('eyesClosed', 0.5 + (0.5 * closed) / a.eyes!.length, `${closed} of ${a.eyes!.length} faces have eyes closed`);
  if (a.faceFraming?.cutFaces.length) add('faceCutOff', 0.4, `${a.faceFraming.cutFaces.length} face(s) cut off by the frame`);
  if (a.horizon?.detected) add('tilted', Math.abs(a.horizon.angleDegrees) / 10, `Horizon tilted ${a.horizon.angleDegrees.toFixed(1)}°`);
  if (a.aesthetics?.isUtility) add('utility', 0.6, 'Looks like a screenshot / document');
  if (a.aesthetics && a.aesthetics.overall < -0.3) add('lowAesthetic', 0.4, `Aesthetic score ${a.aesthetics.overall.toFixed(2)}`);
  if (a.accidental?.likely) add('accidental', 0.9, 'Accidental shot');
  out.sort((x, y) => y.severity - x.severity);
  return { photoId: a.photoId, issues: out };
}

/** Mock near-duplicate groups: the bursts, best (sharpest, eyes open) first. */
export function mockGroups(list: Photo[], bursts: Group[], analysis: (p: Photo) => PhotoAnalysis): SimilarGroup[] {
  return bursts.map((g) => {
    const scored = list.slice(g.start, g.end).map((p) => {
      const a = analysis(p);
      const score = (a.blur?.peak ?? 50) / 100 - (a.eyes?.some((e) => e.closed) ? 0.4 : 0);
      return { id: p.id, score };
    });
    scored.sort((a, b) => b.score - a.score);
    return { photoIds: scored.map((s) => s.id), scores: scored.map((s) => s.score), best: scored[0].id };
  });
}
