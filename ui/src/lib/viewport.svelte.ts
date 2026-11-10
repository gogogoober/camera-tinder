/** True on phone-width windows. The same components switch layout; the logic is shared. */
const query = typeof window !== 'undefined' ? window.matchMedia('(max-width: 760px)') : null;

export const viewport = $state({ compact: query?.matches ?? false });

query?.addEventListener('change', (e) => {
  viewport.compact = e.matches;
});
