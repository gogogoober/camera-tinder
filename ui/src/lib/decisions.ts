import type { Decision } from './api';

export interface DecisionMeta {
  label: string;
  stamp: string;
  icon: string;
  key: string;
  arrow: string;
}

/** ← no · → like · ↑ favorite · ↓ hate */
export const DECISIONS: Record<Decision, DecisionMeta> = {
  dislike: { label: 'Nope', stamp: 'NOPE', icon: '✕', key: 'ArrowLeft', arrow: '←' },
  like: { label: 'Like', stamp: 'LIKE', icon: '♥', key: 'ArrowRight', arrow: '→' },
  favorite: { label: 'Favorite', stamp: 'FAVORITE', icon: '★', key: 'ArrowUp', arrow: '↑' },
  hate: { label: 'Hate', stamp: 'HATE', icon: '☠', key: 'ArrowDown', arrow: '↓' },
};

export const ORDER: Decision[] = ['dislike', 'hate', 'favorite', 'like'];

export function decisionForKey(key: string): Decision | null {
  return (Object.keys(DECISIONS) as Decision[]).find((d) => DECISIONS[d].key === key) ?? null;
}

/** Which decision a drag offset points at (or null if too small). */
export function decisionForDrag(dx: number, dy: number, deadzone = 30): Decision | null {
  const ax = Math.abs(dx);
  const ay = Math.abs(dy);
  if (Math.max(ax, ay) < deadzone) return null;
  if (ax >= ay) return dx > 0 ? 'like' : 'dislike';
  return dy < 0 ? 'favorite' : 'hate';
}
