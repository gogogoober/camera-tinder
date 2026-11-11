<script lang="ts">
  // Back up a card to a drive: pick both, see the plan (folders by brand / year / trip or month),
  // copy with read-back verification, then a report that says plainly whether everything is safe.
  import { onMount } from 'svelte';
  import { api, type CoreEvent, type FolderChoice, type OffloadFolder, type OffloadPlan, type OffloadReport } from '../api';
  import { short, shortRange } from '../days';
  import { formatBytes, formatCount } from '../format';
  import AppShell from './layout/AppShell.svelte';

  let { onBack }: { onBack: () => void } = $props();

  type Stage = 'choose' | 'planning' | 'plan' | 'copying' | 'report';
  const LIBRARY_KEY = 'offload.library';

  let stage = $state<Stage>('choose');
  let card = $state<string | null>(null);
  let library = $state<string | null>(null);
  let fullCheck = $state(false);
  let plan = $state<OffloadPlan | null>(null);
  let choices = $state<FolderChoice[]>([]);
  let report = $state<OffloadReport | null>(null);
  let error = $state<string | null>(null);
  let stopping = $state(false);
  let progress = $state<Extract<CoreEvent, { type: 'offloadProgress' }> | null>(null);
  /** Copy speed, smoothed over the last few seconds: [time ms, bytes] samples. */
  let samples = $state<[number, number][]>([]);
  let editing = $state<string | null>(null);

  try {
    library = localStorage.getItem(LIBRARY_KEY);
  } catch {
    /* storage unavailable: just ask */
  }

  onMount(() => {
    const off = api.onEvent((e) => {
      if (e.type !== 'offloadProgress') return;
      progress = e;
      if (e.phase === 'copy') {
        const now = performance.now();
        samples = [...samples.filter(([t]) => now - t < 5000), [now, e.bytesDone]];
      }
    });
    return () => void off.then((f) => f());
  });

  const PHASE: Record<string, string> = {
    card: 'Reading the card',
    library: 'Checking what the drive already has',
    check: 'Comparing matches byte for byte',
    copy: 'Copying',
  };

  const speed = $derived.by(() => {
    if (samples.length < 2) return 0;
    const [t0, b0] = samples[0];
    const [t1, b1] = samples[samples.length - 1];
    return t1 > t0 ? ((b1 - b0) * 1000) / (t1 - t0) : 0;
  });
  const secondsLeft = $derived(progress && speed > 0 ? (progress.bytesTotal - progress.bytesDone) / speed : null);

  /** Folders by brand, in the order the plan lists them. */
  const byBrand = $derived.by(() => {
    const out = new Map<string, OffloadFolder[]>();
    for (const f of plan?.folders ?? []) out.set(f.brand, [...(out.get(f.brand) ?? []), f]);
    return [...out.entries()];
  });
  const tooBig = $derived(!!plan && plan.newBytes + 2 ** 30 > plan.freeBytes);

  /** Speed of each step and which one set the pace. */
  const stageSpeeds = $derived.by(() => {
    const s = report?.stages;
    if (!s) return null;
    const rate = (b: number, t: number) => (t > 0 && b > 0 ? b / t : null);
    const rows = [
      { label: 'Card read', v: rate(s.readBytes, s.readSeconds) },
      { label: 'Drive write', v: rate(s.writeBytes, s.writeSeconds) },
      { label: 'Check', v: rate(s.checkBytes, s.checkSeconds) },
    ].filter((r): r is { label: string; v: number } => r.v != null);
    const slowest = rows.reduce<{ label: string; v: number } | null>((m, r) => (!m || r.v < m.v ? r : m), null);
    return rows.length ? { rows, slowest: slowest!.label } : null;
  });

  function name(path: string | null) {
    return path ? path.split('/').filter(Boolean).pop() ?? path : '';
  }

  function duration(s: number) {
    if (s < 60) return `${Math.max(1, Math.round(s))} s`;
    const m = Math.round(s / 60);
    return m < 60 ? `${m} min` : `${Math.floor(m / 60)} h ${m % 60} min`;
  }

  async function pickCard() {
    const p = await api.pickOffloadCard();
    if (p) card = p;
  }

  async function pickLibrary() {
    const p = await api.pickOffloadLibrary();
    if (!p) return;
    library = p;
    try {
      localStorage.setItem(LIBRARY_KEY, p);
    } catch {
      /* not remembered: fine */
    }
  }

  async function makePlan() {
    if (!card || !library) return;
    error = null;
    stopping = false;
    progress = null;
    choices = [];
    stage = 'planning';
    try {
      plan = await api.offloadPlan(card, library, fullCheck);
      stage = 'plan';
    } catch (e) {
      error = String(e);
      stage = 'choose';
    }
  }

  async function choose(id: string, change: Partial<FolderChoice> | null) {
    const rest = choices.filter((c) => c.id !== id);
    choices = change ? [...rest, { ...(choices.find((c) => c.id === id) ?? { id }), ...change }] : rest;
    try {
      plan = await api.offloadLayout($state.snapshot(choices));
    } catch (e) {
      error = String(e);
    }
  }

  function rename(f: OffloadFolder, value: string) {
    editing = null;
    const v = value.trim();
    if (!v || v === f.name) return;
    void choose(f.id, { name: v, byMonth: false });
  }

  async function copy() {
    error = null;
    stopping = false;
    progress = null;
    samples = [];
    stage = 'copying';
    try {
      report = await api.offloadStart($state.snapshot(choices));
      stage = 'report';
    } catch (e) {
      error = String(e);
      stage = 'plan';
    }
  }

  async function stop() {
    stopping = true;
    await api.offloadStop();
  }

  function again() {
    plan = null;
    report = null;
    choices = [];
    stage = 'choose';
  }

  const busy = $derived(stage === 'planning' || stage === 'copying');
</script>

<AppShell narrow>
<main>
  <button class="ghost back" onclick={onBack} disabled={busy}>← Back</button>
  <h1>Back up a card</h1>
  <p class="muted">
    Copies every photo and video on the card that isn't on your backup drive yet, wherever it is there, into
    <code>Photos/Brand/Year/Trip</code> folders. Each copy is read back from the drive and checked. Nothing on the card is ever changed.
  </p>

  {#if stage === 'choose' || stage === 'planning'}
    <section class="panel">
      <div class="pick">
        <div>
          <div class="eyebrow">From</div>
          <strong>{card ? name(card) : 'No card chosen'}</strong>
          {#if card}<div class="muted small path">{card}</div>{/if}
        </div>
        <button onclick={pickCard} disabled={busy}>{card ? 'Change' : 'Choose card…'}</button>
      </div>
      <div class="pick">
        <div>
          <div class="eyebrow">To</div>
          <strong>{library ? name(library) : 'No backup drive chosen'}</strong>
          {#if library}<div class="muted small path">{library}</div>{/if}
        </div>
        <button onclick={pickLibrary} disabled={busy}>{library ? 'Change' : 'Choose drive…'}</button>
      </div>
      <label class="check">
        <input type="checkbox" bind:checked={fullCheck} disabled={busy} />
        <span>
          Check files already on the drive byte for byte
          <span class="muted small block">Slower: reads them from the card again. Without it, a match is the file's size, start and end.</span>
        </span>
      </label>

      {#if stage === 'planning'}
        <div class="status">
          <span>{progress ? PHASE[progress.phase] : 'Reading the card'}{progress && progress.total ? ` · ${formatCount(progress.done)} / ${formatCount(progress.total)}` : ''}…</span>
          <button class="small" onclick={stop} disabled={stopping}>Stop</button>
        </div>
        {#if progress?.total}<div class="bar"><div style:width="{(100 * progress.done) / progress.total}%"></div></div>{/if}
        {#if progress?.current}<div class="muted small path">{progress.current}</div>{/if}
      {:else}
        <button class="primary go" onclick={makePlan} disabled={!card || !library}>Find what's new</button>
      {/if}
    </section>
  {/if}

  {#if (stage === 'plan' || stage === 'copying') && plan}
    <section class="panel">
      <div class="summary">
        <div class="big">{formatCount(plan.newFiles)} new {plan.newFiles === 1 ? 'file' : 'files'} · {formatBytes(plan.newBytes)}</div>
        <div class="muted">
          {name(plan.cardRoot)} → {name(plan.libraryRoot)}
          · {formatCount(plan.alreadyFiles)} already on the drive{plan.fullCheck ? ' (checked byte for byte)' : ''}
          · {formatBytes(plan.freeBytes)} free
        </div>
        {#if plan.alreadyIn.length}
          <details class="muted small">
            <summary>Where the {formatCount(plan.alreadyFiles)} already on the drive were found</summary>
            <ul class="found">
              {#each plan.alreadyIn.slice(0, 40) as a (a.folder)}<li><span class="path">{a.folder || '(top of the folder you picked)'}</span><span>{formatCount(a.files)}</span></li>{/each}
            </ul>
            {#if plan.alreadyIn.length > 40}<div>…and {formatCount(plan.alreadyIn.length - 40)} more folders.</div>{/if}
          </details>
        {/if}
        {#if plan.lastBackups.length}
          <div class="muted small">
            Last backups to this drive:
            {#each plan.lastBackups as b, i (b.brand)}{i ? ' · ' : ''}{b.brand} {b.lastDay ? `up to ${short(b.lastDay)}` : ''} (on {short(b.at.slice(0, 10))}){/each}
          </div>
        {/if}
        {#if plan.skipped.length}
          <div class="muted small">
            Not copied (not photos or videos):
            {plan.skipped.map((s) => `${formatCount(s.count)} ${s.extension}`).join(', ')}
          </div>
        {/if}
      </div>
      {#each plan.warnings as w (w)}<p class="warn">{w}</p>{/each}
      {#if plan.unreadable.length}
        <div class="warn">
          {plan.unreadable.length} {plan.unreadable.length === 1 ? 'file' : 'files'} on the card couldn't be read and won't be copied:
          <ul>{#each plan.unreadable.slice(0, 10) as f (f.path)}<li>{f.path}: {f.error}</li>{/each}</ul>
        </div>
      {/if}

      {#if plan.newFiles === 0}
        <p class="ok">✓ Everything on this card is already on the drive.</p>
      {/if}

      {#each byBrand as [brand, folders] (brand)}
        <div class="eyebrow">{brand}</div>
        <ul class="folders">
          {#each folders as f (f.id)}
            <li>
              <div class="row">
                {#if editing === f.id}
                  <!-- svelte-ignore a11y_autofocus -->
                  <input
                    class="rename"
                    value={f.name}
                    autofocus
                    onblur={(e) => rename(f, e.currentTarget.value)}
                    onkeydown={(e) => {
                      if (e.key === 'Enter') rename(f, e.currentTarget.value);
                      if (e.key === 'Escape') editing = null;
                    }}
                  />
                {:else}
                  <button class="plain fname" title="Rename" onclick={() => (editing = f.id)} disabled={stage !== 'plan'}>
                    <span class="muted">{f.year ?? ''}/</span>{f.name}
                  </button>
                {/if}
                <span class="muted small">{f.exists ? 'adds to existing folder' : 'new folder'}</span>
                <span class="size">{formatBytes(f.bytes)}</span>
              </div>
              <div class="muted small">
                {f.firstDay && f.lastDay ? shortRange(f.firstDay, f.lastDay) : 'No date'}
                · {[f.jpeg && `${formatCount(f.jpeg)} JPEG`, f.raw && `${formatCount(f.raw)} RAW`, f.heif && `${formatCount(f.heif)} HIF`, f.video && `${formatCount(f.video)} video`].filter(Boolean).join(' · ')}
              </div>
              {#if stage === 'plan' && (f.options.length || f.isTrip || f.splitFrom.length)}
                <div class="opts">
                  {#each f.options as o (o)}<button class="chip" onclick={() => choose(f.id, { name: o, byMonth: false })}>Name it “{o}”</button>{/each}
                  {#if f.isTrip}<button class="chip" onclick={() => choose(f.id, { byMonth: true, name: null })}>Split by month</button>{/if}
                  {#each f.splitFrom as [id, trip] (id)}<button class="chip" onclick={() => choose(id, null)}>Back to “{trip}”</button>{/each}
                </div>
              {/if}
            </li>
          {/each}
        </ul>
      {/each}

      {#if stage === 'plan'}
        <div class="actions">
          <button class="ghost" onclick={again}>Start over</button>
          <button class="primary go" onclick={copy} disabled={plan.newFiles === 0 || tooBig}>
            Copy {formatCount(plan.newFiles)} {plan.newFiles === 1 ? 'file' : 'files'}
          </button>
        </div>
      {:else}
        <div class="copying">
          <div class="status">
            <span>
              {stopping ? 'Stopping after the files already copied are checked…' : `${formatCount(progress?.done ?? 0)} of ${formatCount(plan.newFiles)} copied and checked`}
            </span>
            <button class="small" onclick={stop} disabled={stopping}>Stop</button>
          </div>
          <div class="bar"><div style:width="{progress?.bytesTotal ? (100 * progress.bytesDone) / progress.bytesTotal : 0}%"></div></div>
          <div class="muted small">
            {formatBytes(progress?.bytesDone ?? 0)} of {formatBytes(plan.newBytes)}
            {#if speed > 0} · {formatBytes(speed)}/s{/if}
            {#if secondsLeft != null} · about {duration(secondsLeft)} left{/if}
          </div>
          {#if progress?.current}<div class="muted small path">{progress.current}</div>{/if}
          <p class="muted small">Your Mac stays awake until this finishes. Stop is safe: copies already checked stay, the file in progress is removed, and running it again picks up the rest.</p>
        </div>
      {/if}
    </section>
  {/if}

  {#if stage === 'report' && report}
    <section class="panel">
      {#if report.allSafe}
        <p class="ok big">✓ Every photo and video on this card is on the drive, read back and matched.</p>
      {:else}
        <p class="bad big">
          {report.error ? `Stopped: ${report.error}.` : report.stopped ? 'Stopped before the end.' : 'Not everything on the card is backed up.'}
        </p>
      {/if}
      <div>
        Copied {formatCount(report.copied)} {report.copied === 1 ? 'file' : 'files'} ({formatBytes(report.bytes)}) in {duration(report.seconds)}.
        {#if report.already}<span class="muted">{formatCount(report.already)} were already on the drive.</span>{/if}
      </div>
      {#if report.skipped.length}
        <div class="muted small">Not copied (not photos or videos): {report.skipped.map((s) => `${formatCount(s.count)} ${s.extension}`).join(', ')}</div>
      {/if}
      {#if report.failed.length}
        <div class="bad">{report.failed.length} failed:</div>
        <ul class="bad list">{#each report.failed.slice(0, 30) as f (f.path)}<li>{f.path}: {f.error}</li>{/each}</ul>
      {/if}
      {#if report.notBackedUp.length}
        <div class="bad">{formatCount(report.notBackedUp.length)} on the card {report.notBackedUp.length === 1 ? 'is' : 'are'} not on the drive yet:</div>
        <ul class="bad list">{#each report.notBackedUp.slice(0, 30) as p (p)}<li>{p}</li>{/each}</ul>
        {#if report.notBackedUp.length > 30}<div class="muted small">…and {formatCount(report.notBackedUp.length - 30)} more.</div>{/if}
      {/if}
      {#if report.recheckError}<p class="bad">Couldn't re-check the card at the end: {report.recheckError}</p>{/if}
      {#if stageSpeeds}
        <div class="muted small">
          {#each stageSpeeds.rows as r, i (r.label)}{i ? ' · ' : ''}<span class:slowest={r.label === stageSpeeds.slowest}>{r.label} {formatBytes(r.v)}/s</span>{/each}
          · the {stageSpeeds.slowest.toLowerCase()} set the pace.
        </div>
      {/if}
      {#if report.manifest}<div class="muted small">Hash list: <code>{report.manifest}</code></div>{/if}
      <div class="actions">
        <button class="ghost" onclick={onBack}>Done</button>
        <button class="primary" onclick={again}>{report.allSafe ? 'Back up another card' : 'Plan again'}</button>
      </div>
    </section>
  {/if}

  {#if error}<p class="bad">{error}</p>{/if}
</main>
</AppShell>

<style>
  main { padding: 32px 24px; overflow-y: auto; }
  .back { color: var(--muted); margin-bottom: 16px; }
  h1 { margin: 0 0 6px; }
  code { font-size: 12.5px; word-break: break-all; }
  .panel { background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius); padding: 20px; margin-top: 20px; display: grid; gap: 14px; }
  .pick { display: flex; justify-content: space-between; align-items: center; gap: 12px; }
  .pick > div { min-width: 0; }
  .path { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; user-select: text; -webkit-user-select: text; }
  .eyebrow { font: 600 11px ui-monospace, 'SF Mono', monospace; letter-spacing: 0.08em; text-transform: uppercase; color: var(--muted); }
  .check { display: flex; gap: 10px; align-items: flex-start; }
  .check input { width: 18px; height: 18px; margin-top: 2px; accent-color: var(--accent); }
  .block { display: block; }
  .small { font-size: 12.5px; }
  .go { padding: 12px; font-size: 16px; }
  .status { display: flex; justify-content: space-between; align-items: center; gap: 12px; }
  .bar { height: 6px; border-radius: 99px; background: var(--panel-2); overflow: hidden; }
  .bar > div { height: 100%; background: var(--accent); transition: width 0.2s; }
  .summary { display: grid; gap: 4px; }
  .big { font-size: 18px; font-weight: 700; }
  .warn { color: var(--favorite); margin: 0; }
  .ok { color: var(--like); margin: 0; font-weight: 600; }
  .bad { color: var(--hate); margin: 0; }
  .list, .warn ul { margin: 4px 0 0; padding-left: 18px; font-size: 13px; user-select: text; -webkit-user-select: text; }
  .folders { list-style: none; margin: -6px 0 0; padding: 0; display: grid; gap: 8px; }
  .folders li { border: 1px solid var(--border); border-radius: 10px; padding: 10px 12px; display: grid; gap: 4px; }
  .row { display: flex; align-items: baseline; gap: 10px; }
  .fname { font-weight: 600; font-size: 15px; padding: 0; text-align: left; }
  .fname:not(:disabled):hover { text-decoration: underline dotted; }
  .rename { font: inherit; font-weight: 600; padding: 2px 6px; border-radius: 6px; border: 1px solid var(--accent); min-width: 0; flex: 1; }
  .size { margin-left: auto; font-variant-numeric: tabular-nums; }
  .opts { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 2px; }
  .chip { padding: 3px 10px; border-radius: 99px; border: 1px solid var(--border); background: transparent; font-size: 12.5px; }
  .actions { display: flex; justify-content: flex-end; gap: 10px; }
  .copying { display: grid; gap: 8px; }
  .slowest { color: var(--text); font-weight: 600; }
  .found { list-style: none; margin: 4px 0 0; padding: 0; display: grid; gap: 2px; }
  .found li { display: flex; justify-content: space-between; gap: 12px; }
  summary { cursor: pointer; }
</style>
