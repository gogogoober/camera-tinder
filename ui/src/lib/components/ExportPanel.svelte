<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type Decision, type ExportReport, type SavedTag } from '../api';
  import { daysLabel } from '../days';
  import { formatCount } from '../format';
  import AppShell from './layout/AppShell.svelte';

  let {
    onBack,
    days = null,
    preset = null,
  }: {
    onBack: () => void;
    /** The days the screen you came from was showing (the Pick selection or one Review day). */
    days?: string[] | null;
    /** Decisions to start with ticked (Review passes the filter you were looking at). */
    preset?: Decision[] | null;
  } = $props();

  const KINDS: { d: Decision; label: string; color: string }[] = [
    { d: 'favorite', label: '★ Favorites', color: 'var(--favorite)' },
    { d: 'like', label: '✓ Keep', color: 'var(--like)' },
    { d: 'dislike', label: '✕ Nope', color: 'var(--muted)' },
    { d: 'hate', label: '⊘ Hate', color: 'var(--hate)' },
  ];

  // svelte-ignore state_referenced_locally
  let chosen = $state(new Set<Decision>(preset ?? ['favorite', 'like']));
  // svelte-ignore state_referenced_locally
  let useDays = $state(!!days);
  /** Off by default: photos exported before are left out. */
  let includeExported = $state(false);
  /** How many matching photos were exported before (what the checkbox would add). */
  let previouslyExported = $state(0);
  /** Bumped after each export so the counts refresh. */
  let exported = $state(0);
  let tags = $state<SavedTag[]>([]);
  let pickedTags = $state(new Set<number>());
  let count = $state(0);
  let running = $state(false);
  let progress = $state({ done: 0, total: 0 });
  let report = $state<ExportReport | null>(null);
  let error = $state<string | null>(null);

  /** Union of the ticked scopes; null = every day. */
  const scope = $derived.by(() => {
    const out = new Set<string>(useDays && days ? days : []);
    for (const t of tags) if (pickedTags.has(t.id)) t.days.forEach((d) => out.add(d));
    return useDays || pickedTags.size ? [...out] : null;
  });
  const options = $derived({
    includeFavorites: chosen.has('favorite'),
    includeLikes: chosen.has('like'),
    includeDislikes: chosen.has('dislike'),
    includeHates: chosen.has('hate'),
    includeExported,
    days: scope,
  });

  onMount(() => {
    api.listTags().then((t) => (tags = t));
    const off = api.onEvent((e) => {
      if (e.type === 'exportProgress') progress = { done: e.done, total: e.total };
    });
    return () => void off.then((f) => f());
  });

  // Live count for the current choice (ignore answers that arrive out of order).
  let ticket = 0;
  /** Opened when everything in scope was exported before (Review's "Export again"): start with them included. */
  let firstCount = true;
  $effect(() => {
    const o = $state.snapshot(options);
    const mine = ++ticket;
    void exported; // re-count after an export
    Promise.all([api.exportCount({ ...o, includeExported: true }), api.exportCount({ ...o, includeExported: false })]).then(([all, fresh]) => {
      if (mine !== ticket) return;
      if (firstCount && fresh === 0 && all > 0 && !o.includeExported) includeExported = true;
      firstCount = false;
      count = o.includeExported ? all : fresh;
      previouslyExported = all - fresh;
      error = null;
    }).catch((e) => {
      if (mine === ticket) error = String(e);
    });
  });

  function toggle(d: Decision) {
    const next = new Set(chosen);
    if (next.has(d)) next.delete(d);
    else next.add(d);
    chosen = next;
  }
  function toggleTag(id: number) {
    const next = new Set(pickedTags);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    pickedTags = next;
  }

  async function run() {
    error = null;
    report = null;
    try {
      const dest = await api.pickExportFolder();
      if (!dest) return;
      running = true;
      progress = { done: 0, total: count };
      report = await api.exportKeepers(dest, $state.snapshot(options));
      exported++;
    } catch (e) {
      error = String(e);
    } finally {
      running = false;
    }
  }
</script>

<AppShell narrow>
<main>
  <button class="ghost back" onclick={onBack} disabled={running}>← Back</button>
  <h1>Export</h1>
  <p class="muted">Copies the original files (never previews) for the decisions and days you choose. Originals are never moved or deleted, and running it again only copies what's new.</p>

  <section class="panel">
    <div class="eyebrow">Decisions</div>
    {#each KINDS as k (k.d)}
      <label>
        <input type="checkbox" checked={chosen.has(k.d)} onchange={() => toggle(k.d)} disabled={running} />
        <span style:color={k.color}>{k.label}</span>
      </label>
    {/each}

    <div class="eyebrow">Days</div>
    <div class="chips">
      {#if days}
        <button class="chip" class:on={useDays} aria-pressed={useDays} onclick={() => (useDays = !useDays)} disabled={running}>
          {daysLabel(days)}
        </button>
      {/if}
      {#each tags as t (t.id)}
        <button class="chip" class:on={pickedTags.has(t.id)} aria-pressed={pickedTags.has(t.id)} onclick={() => toggleTag(t.id)} disabled={running}>{t.name}</button>
      {/each}
      {#if !days && !tags.length}<span class="muted">All days (save tags on the Pick screen to narrow this down).</span>{/if}
    </div>
    <div class="muted small">{scope ? 'Only the ticked days and tags.' : 'Nothing ticked: every day.'}</div>

    <label class="inc">
      <input type="checkbox" bind:checked={includeExported} disabled={running} />
      <span>Include photos exported before</span>
      <span class="muted">{formatCount(previouslyExported)} {includeExported ? 'included' : 'left out'}</span>
    </label>

    <button class="primary go" onclick={run} disabled={running || count === 0}>
      Choose folder & copy {formatCount(count)} photos
    </button>

    {#if running}
      <div class="progress"><div style:width="{progress.total ? (100 * progress.done) / progress.total : 0}%"></div></div>
      <div class="muted">{formatCount(progress.done)} / {formatCount(progress.total)}</div>
    {/if}

    {#if report}
      <div class="report">
        <strong>Done.</strong> Copied {formatCount(report.copied)} to <code>{report.destination}</code>.
        {#if report.notMarked}<div class="error">The copies are fine, but they couldn't be recorded as exported ({report.notMarked}), so they may be copied again next time.</div>{/if}
        {#if report.alreadyThere}<span class="muted">{formatCount(report.alreadyThere)} were already there.</span>{/if}
        {#if report.failed.length}
          <div class="error">{report.failed.length} failed:</div>
          <ul class="error">
            {#each report.failed.slice(0, 20) as [path, why] (path)}<li>{path}: {why}</li>{/each}
          </ul>
        {/if}
      </div>
    {/if}
    {#if error}<p class="error">{error}</p>{/if}
  </section>

  <p class="muted soon">Coming in v2: send to Lightroom and more.</p>
</main>
</AppShell>

<style>
  main { padding: 32px 24px; overflow-y: auto; }
  .back { color: var(--muted); margin-bottom: 16px; }
  h1 { margin: 0 0 6px; }
  .panel { background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius); padding: 20px; margin-top: 20px; display: grid; gap: 14px; }
  label { display: flex; gap: 10px; align-items: center; font-weight: 600; }
  input[type='checkbox'] { width: 18px; height: 18px; accent-color: var(--accent); }
  .go { padding: 12px; font-size: 16px; }
  .report { line-height: 1.6; user-select: text; -webkit-user-select: text; }
  code { font-size: 13px; word-break: break-all; }
  ul { margin: 4px 0 0; padding-left: 18px; font-size: 13px; }
  .eyebrow { font: 600 11px ui-monospace, 'SF Mono', monospace; letter-spacing: 0.08em; text-transform: uppercase; color: var(--muted); }
  .small { font-size: 12.5px; }
  .inc { font-weight: 500; }
  .inc .muted { margin-left: auto; font-weight: 400; }
  .chips { display: flex; flex-wrap: wrap; gap: 8px; }
  .chip { padding: 5px 12px; border-radius: 99px; border: 1px solid var(--border); background: transparent; font-size: 13px; }
  .chip.on { background: var(--accent); border-color: var(--accent); color: #fff; }
  .soon { margin-top: 16px; font-size: 13px; }
</style>
