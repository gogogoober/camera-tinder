// Mirrors the serde types in crates/cull-core/src/analysis/. See docs/photo-analysis.md.
// All rectangles/points are fractions (0–1) of the upright image, origin top-left.

import type { Filter } from './types';

export type AnalysisKind =
  | 'blur'
  | 'exposure'
  | 'contrast'
  | 'noise'
  | 'colorCast'
  | 'accidental'
  | 'perceptualHash'
  | 'similarity'
  | 'faces'
  | 'eyes'
  | 'faceFraming'
  | 'faceQuality'
  | 'saliency'
  | 'subjectFocus'
  | 'aesthetics'
  | 'horizon';

export interface NormRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface NormPoint {
  x: number;
  y: number;
}

export interface Blur {
  /** Whole-frame sharpness 0–100 (same scale as Photo.sharpness). */
  score: number;
  /** Sharpness of the sharpest 10% of the frame, 0–100. */
  peak: number;
  /** 0 = edges in all directions, 1 = all aligned (motion blur). */
  directionality: number;
  verdict: 'sharp' | 'soft' | 'motionBlur';
}

export interface Exposure {
  mean: number;
  shadowsClipped: number;
  highlightsClipped: number;
  p05: number;
  p95: number;
  verdict: 'ok' | 'underexposed' | 'overexposed';
}

export interface Contrast {
  rms: number;
  range: number;
  flat: boolean;
}

export interface Noise {
  /** Noise standard deviation, 0–255 scale (measured on the 2048 px preview). */
  sigma: number;
  level: 'low' | 'medium' | 'high';
}

export interface ColorCast {
  warmCool: number;
  greenMagenta: number;
  strength: number;
  hue: 'neutral' | 'warm' | 'cool' | 'green' | 'magenta';
}

export interface Accidental {
  likely: boolean;
  reason: 'dark' | 'blank' | null;
}

export interface Embedding {
  model: string;
  vector: number[];
}

export interface Face {
  bounds: NormRect;
  confidence: number;
  roll: number | null;
  yaw: number | null;
  pitch: number | null;
  leftEye: NormPoint[];
  rightEye: NormPoint[];
}

export interface EyeState {
  /** Index into PhotoAnalysis.faces. */
  face: number;
  leftOpenness: number | null;
  rightOpenness: number | null;
  closed: boolean;
}

export interface FaceFraming {
  cutFaces: number[];
}

export interface FaceQuality {
  bounds: NormRect;
  /** 0–1, compare between shots of the same person. */
  quality: number;
}

export interface SalientRegion {
  bounds: NormRect;
  confidence: number;
}

export interface SubjectFocus {
  subject: number | null;
  frame: number;
  verdict: 'inFocus' | 'subjectSoft' | 'backgroundFocused' | 'noSubject';
}

export interface Aesthetics {
  /** Roughly −1…1 (Apple Vision). */
  overall: number;
  isUtility: boolean;
}

export interface Horizon {
  detected: boolean;
  angleDegrees: number;
  confidence: number;
}

/** Every stored result for one photo; a field is null until computed (or if it failed: see errors). */
export interface PhotoAnalysis {
  photoId: number;
  blur: Blur | null;
  exposure: Exposure | null;
  contrast: Contrast | null;
  noise: Noise | null;
  colorCast: ColorCast | null;
  accidental: Accidental | null;
  /** 16 hex chars (64-bit dHash). */
  perceptualHash: string | null;
  /** Only filled by photoAnalysis(id); lists omit it. */
  similarity: Embedding | null;
  faces: Face[] | null;
  eyes: EyeState[] | null;
  faceFraming: FaceFraming | null;
  faceQuality: FaceQuality[] | null;
  saliency: SalientRegion[] | null;
  subjectFocus: SubjectFocus | null;
  aesthetics: Aesthetics | null;
  horizon: Horizon | null;
  implementations: Partial<Record<AnalysisKind, string>>;
  errors: Partial<Record<AnalysisKind, string>>;
}

export interface AnalyzerInfo {
  kind: AnalysisKind;
  /** null when no engine is installed on this platform. */
  implementation: string | null;
  inputs: AnalysisKind[];
}

export interface AnalyzeRequest {
  photoIds?: number[] | null;
  /** null/omitted (and no photoIds) = every photo in the session. */
  filter?: Filter | null;
  /** Empty/omitted = every available kind. */
  kinds?: AnalysisKind[];
  force?: boolean;
}

export interface AnalyzeReport {
  photos: number;
  analyzed: number;
  upToDate: number;
  kindErrors: number;
  failed: [number, string][];
}

export type IssueKind =
  | 'blurry'
  | 'motionBlur'
  | 'subjectOutOfFocus'
  | 'underexposed'
  | 'overexposed'
  | 'flat'
  | 'noisy'
  | 'colorCast'
  | 'eyesClosed'
  | 'faceCutOff'
  | 'poorFaceQuality'
  | 'tilted'
  | 'lowAesthetic'
  | 'utility'
  | 'accidental';

export interface Issue {
  kind: IssueKind;
  /** 0–1, for sorting. */
  severity: number;
  detail: string;
}

export interface PhotoIssues {
  photoId: number;
  /** Most severe first. */
  issues: Issue[];
}

export interface GroupingOptions {
  /** null = no time limit. Default 600. */
  maxGapSecs?: number | null;
  /** Default 10 (of 64). */
  maxHashBits?: number;
  useEmbeddings?: boolean;
  /** Multiplier on the calibrated embedding threshold. Default 1. */
  embeddingTolerance?: number;
  window?: number;
}

export interface SimilarGroup {
  /** Best first. */
  photoIds: number[];
  scores: number[];
  best: number;
}
