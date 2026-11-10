import { flushSync } from 'svelte';

/**
 * Run a state change that reshapes the page (a rail appearing or leaving) as a view transition:
 * AppShell's regions carry `view-transition-name`s, so the main surface glides to its new size and
 * the rails fade/slide (timings in app.css). Falls back to an instant change where the API is
 * missing or the user prefers reduced motion.
 */
export function withLayoutTransition(update: () => void) {
  const reduce = typeof matchMedia !== 'undefined' && matchMedia('(prefers-reduced-motion: reduce)').matches;
  if (reduce || typeof document === 'undefined' || !('startViewTransition' in document)) {
    update();
    return;
  }
  // A view transition waits for a rendered frame before it applies the change. A hidden, covered or
  // minimised window may draw none, so never let the layout wait on the animation.
  let done = false;
  const run = () => {
    if (done) return;
    done = true;
    update();
  };
  const t = document.startViewTransition(() => flushSync(run));
  setTimeout(() => {
    if (done) return;
    t.skipTransition();
    run();
  }, 500);
}
