<script>
  import { route } from '../lib/router.js';
  import { scenesState } from '../lib/scenes.svelte.js';
  import { monitorState } from '../lib/browserMonitor.js';
  import {
    micPref,
    micMode,
    micToggleMuted,
    micPttPressed,
    micEffectiveMuted,
    persistToggleMuted,
  } from '../lib/browserMic.js';
  import { graph, getClientId } from '../lib/stores.js';
  import { micState } from '../lib/browserMic.js';
  import { audioAncillary } from '../lib/audioClaim.js';
  import HealthDot from './HealthDot.svelte';

  const clientId = getClientId();
  // The mute button is only meaningful when capture is actually running
  // — before the browser's audio-autoplay gesture unlocks getUserMedia
  // (or if the Sources row is Off) there's nothing to mute. Grey the
  // button in either case; it snaps back to its persisted state only
  // once `micState.capture === 'active'`.
  const micRouted = $derived(
    !!($graph?.web_mic_sources ?? []).find((w) => w.client_uuid === clientId)?.routed_to
  );
  const micActive = $derived($micState?.capture === 'active');
  const micIdle = $derived(!micRouted || !micActive);

  // Menu structure: top-level items are either simple links or dropdown
  // openers. An item with `children` has no route of its own — its label is
  // just a menu opener and every destination lives inside the dropdown.
  const items = [
    {
      label: 'Project',
      children: [
        { href: '#/projects',   label: 'Load',       match: (r) => r === '/' || r === '/projects' },
        { href: '#/characters', label: 'Characters', match: (r) => r === '/characters' },
        { href: '#/dictionary', label: 'Dictionary', match: (r) => r === '/dictionary' },
        { href: '#/scenes',     label: 'Scenes',     match: (r) => r === '/scenes' || r === '/speak' },
        { href: '#/script',     label: 'Scripts',    match: (r) => r === '/script' || r === '/chat' || r.startsWith('/script/') || r.startsWith('/chat/') },
        { href: '#/settings',   label: 'Settings',   match: (r) => r === '/settings' },
      ],
    },
    { href: '#/mixer',    label: 'Mixer',    match: (r) => r === '/mixer' },
    { href: '#/record',   label: 'Record',   match: (r) => r === '/record' },
  ];

  // A parent is "active" when any of its children match — so the Project
  // chip stays highlighted while the user is inside Characters/Scripts/etc.
  function parentActive(item, r) {
    return item.children?.some((c) => c.match(r)) ?? false;
  }

  // Label of the currently-routed child, shown as a bold suffix in the
  // parent pill ("Project: Script"). Null when no child matches.
  function activeSuffix(item, r) {
    return item.children?.find((c) => c.match(r))?.label ?? null;
  }

  // Open state has two independent sources:
  //  - openIndex: click-toggled via the caret. Sticky until closed.
  //  - hoveredIndex: set on mouseenter, cleared on mouseleave. Ephemeral.
  // We intentionally do NOT use CSS :hover to open the dropdown. After a
  // link click the cursor is still over the item, and browsers can dispatch
  // stray hover events during click/navigation — that would pop the menu
  // back open. Requiring a fresh mouseenter guarantees the cursor actually
  // left and returned before we reopen.
  let openIndex = $state(-1);
  let hoveredIndex = $state(-1);

  // After a link click, block the next mouseenter for that item until the
  // cursor leaves. Without this, the cursor is still hovering the item and
  // would immediately re-open the dropdown on the next mousemove.
  let suppressedIndex = $state(-1);

  function isItemOpen(i) {
    return openIndex === i || hoveredIndex === i;
  }

  function toggleOpen(i) {
    openIndex = openIndex === i ? -1 : i;
  }

  function closeAll() {
    openIndex = -1;
  }

  function handleLinkClick(i, sameRoute) {
    // Clicking a link that points at the page you're already on isn't a
    // navigation — force the dropdown open and clear any prior suppression
    // (e.g. left over from the click that landed you on this route while
    // still hovering the button). It will close naturally on mouseleave.
    if (sameRoute) {
      suppressedIndex = -1;
      hoveredIndex = i;
      return;
    }
    openIndex = -1;
    hoveredIndex = -1;
    suppressedIndex = i;
  }

  function handleMouseEnter(i) {
    if (suppressedIndex !== i) hoveredIndex = i;
  }

  function handleMouseLeave(i) {
    if (hoveredIndex === i) hoveredIndex = -1;
    if (suppressedIndex === i) suppressedIndex = -1;
  }

  const currentProject = $derived(
    scenesState.selectedProjectId
      ? scenesState.projects.find((p) => p.id === scenesState.selectedProjectId) ?? null
      : null,
  );

  // The browser monitor auto-restored from localStorage on this page load,
  // but the AudioContext is still suspended because Web Audio autoplay
  // policy needs a user gesture. Any click/keydown on the page will do it
  // (a listener in browserMonitor.js is watching), so this is purely a
  // hint — no click handler needed on the banner itself.
  const awaitingGesture = $derived($monitorState === 'awaiting-gesture');

  // Mic mute button. Visible only when the Web microphone input toggle
  // is on (otherwise there's nothing to mute). Two behaviors:
  //   * `toggle` mode — click flips the sticky `micToggleMuted` store.
  //   * `ptt` mode    — mic is muted unless the user is actively holding
  //                     the button (pointerdown → pointerup / cancel /
  //                     leave, plus keyboard Space for accessibility).
  function onMuteClick() {
    if (micIdle) return; // disabled — either not routed or capture not yet active
    if ($micMode === 'toggle') persistToggleMuted(!$micToggleMuted);
    // In PTT mode a plain click is a no-op — the pointerdown/pointerup
    // handlers below drive the transient state.
  }
  function onMutePointerDown(ev) {
    if (micIdle) return;
    if ($micMode !== 'ptt') return;
    // Only left button. Capture the pointer so pointerup fires here
    // even if the cursor slides off the button while held.
    if (ev.button !== 0 && ev.pointerType === 'mouse') return;
    ev.currentTarget.setPointerCapture?.(ev.pointerId);
    micPttPressed.set(true);
  }
  function onMutePointerUp(ev) {
    if ($micMode !== 'ptt') return;
    try { ev.currentTarget.releasePointerCapture?.(ev.pointerId); } catch {}
    micPttPressed.set(false);
  }
  function onMuteKeyDown(ev) {
    if (micIdle) return;
    if ($micMode !== 'ptt') return;
    if (ev.key !== ' ' && ev.key !== 'Spacebar') return;
    if (ev.repeat) return;
    ev.preventDefault();
    micPttPressed.set(true);
  }
  function onMuteKeyUp(ev) {
    if ($micMode !== 'ptt') return;
    if (ev.key !== ' ' && ev.key !== 'Spacebar') return;
    ev.preventDefault();
    micPttPressed.set(false);
  }
</script>

<svelte:window onclick={closeAll} />

<nav class="menubar">
  {#if $micPref && !$audioAncillary}
    <button
      type="button"
      class="mic-btn"
      class:muted={$micEffectiveMuted}
      class:ptt={$micMode === 'ptt'}
      class:pressed={$micMode === 'ptt' && $micPttPressed}
      class:idle={micIdle}
      disabled={micIdle}
      onclick={onMuteClick}
      onpointerdown={onMutePointerDown}
      onpointerup={onMutePointerUp}
      onpointercancel={onMutePointerUp}
      onpointerleave={onMutePointerUp}
      onkeydown={onMuteKeyDown}
      onkeyup={onMuteKeyUp}
      aria-pressed={$micEffectiveMuted ? 'false' : 'true'}
      title={!micRouted
        ? 'Web mic not routed — pick a destination in Mixer › Sources'
        : (!micActive
          ? 'Web mic waiting for a page interaction to unlock audio capture'
          : ($micMode === 'ptt'
            ? ($micPttPressed ? 'Push-to-talk — transmitting' : 'Push-to-talk — hold to transmit')
            : ($micToggleMuted ? 'Web mic muted (click to unmute)' : 'Web mic live (click to mute)')))}
    >
      <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
        <!-- Microphone body + stand -->
        <path d="M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3z"
              fill="currentColor"/>
        <path d="M5 11a7 7 0 0 0 14 0M12 18v3M8 21h8"
              fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
        {#if $micEffectiveMuted}
          <!-- Slash for muted state -->
          <line x1="4" y1="4" x2="20" y2="20"
                stroke="currentColor" stroke-width="2.5" stroke-linecap="round"/>
        {/if}
      </svg>
    </button>
  {/if}
  <a class="brand" href="#/">RPG Vox</a>
  <ul>
    {#each items as it, i (it.href)}
      <li
        class="menu-item"
        class:has-children={!!it.children}
        onmouseenter={() => handleMouseEnter(i)}
        onmouseleave={() => handleMouseLeave(i)}
      >
        {#if it.children}
          {@const suffix = activeSuffix(it, $route)}
          <button
            type="button"
            class="parent-row"
            class:open={isItemOpen(i)}
            class:active={parentActive(it, $route)}
            aria-haspopup="menu"
            aria-expanded={isItemOpen(i)}
            onclick={(e) => { e.stopPropagation(); toggleOpen(i); }}
          >
            <span class="label">{it.label}{#if suffix}: <strong>{suffix}</strong>{/if}</span>
            <span class="caret" aria-hidden="true">
              <svg viewBox="0 0 10 6" width="10" height="6">
                <path d="M1 1l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
              </svg>
            </span>
          </button>
          <ul class="dropdown" class:visible={isItemOpen(i)}>
            {#each it.children as c (c.href)}
              <li>
                <a
                  href={c.href}
                  class:active={c.match($route)}
                  onclick={() => handleLinkClick(i, c.match($route))}
                >{c.label}</a>
              </li>
            {/each}
          </ul>
        {:else}
          <a href={it.href} class:active={it.match($route)}>{it.label}</a>
        {/if}
      </li>
    {/each}
  </ul>
  {#if awaitingGesture}
    <!-- Absolute-positioned so it can center over the menubar without
         disrupting the flex layout of brand/menu/status. The listener
         installed in browserMonitor.js picks up the click anywhere on
         the page, so this element is purely informational. -->
    <div class="gesture-banner" aria-live="polite">
      click anywhere to resume audio session
    </div>
  {/if}
  <div class="status">
    <a class="project" href="#/projects" title="Change project">
      {#if currentProject}
        <span class="proj-label">Project</span>
        <span class="proj-name">{currentProject.name}</span>
      {:else}
        <span class="proj-none">No project loaded</span>
      {/if}
    </a>
    <HealthDot />
  </div>
</nav>

<style>
  .menubar {
    display: flex;
    align-items: center;
    gap: 20px;
    /* Zero vertical padding — the menu items themselves carry
       6px top/bottom, which is enough click target. Kept horizontal
       padding so the brand + last item don't collide with the edge. */
    padding: 0 20px;
    background: var(--panel);
    border-bottom: 1px solid var(--border);
    position: sticky;
    top: 0;
    z-index: 10;
  }
  .brand {
    font-weight: 700;
    color: var(--text);
    text-decoration: none;
    font-size: 15px;
  }
  /* Square mic-mute button. Rendered as a top-left corner tab that
     stacks directly above `.drawer-hamburger` (which lives fixed at
     `top: var(--menubar-height); left: 0`) — same 34 px width and same
     rounded-bottom-right shape so the pair reads as one column of
     corner buttons. Colors track the *effective* mute state: green
     when transmitting (unmuted / PTT held), red when silent.

     `margin-left: -20px` eats the menubar's 20 px left padding so the
     button is flush against the viewport edge; the flex `gap: 20px`
     on the menubar keeps the brand a comfortable distance away. */
  .mic-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    margin: 0 0 0 -20px;
    width: 34px;
    height: var(--menubar-height);
    padding: 0;
    background: rgba(255,255,255,0.04);
    color: #6fd08c;
    /* Match `.drawer-hamburger`: no top/left border, rounded only at
       the bottom-right corner so the two tabs stack seamlessly. */
    border: 1px solid rgba(111,208,140,0.5);
    border-top: none;
    border-left: none;
    border-radius: 0 0 6px 0;
    cursor: pointer;
    /* Touch-action prevents mobile browsers from interpreting a hold
       as a scroll gesture — required for PTT to feel responsive. */
    touch-action: none;
    /* Prevent the browser's tap-hold selection popover from firing
       during PTT holds. */
    user-select: none;
    -webkit-user-select: none;
  }
  .mic-btn:hover { background: rgba(255,255,255,0.08); }
  .mic-btn.muted {
    color: #e35555;
    border-color: rgba(227,85,85,0.55);
    background: rgba(227,85,85,0.08);
  }
  .mic-btn.muted:hover { background: rgba(227,85,85,0.16); }
  /* Push-to-talk mode: while pressed, treat as "live-transmit" green
     and add a subtle inset shadow so the button visibly depresses. */
  .mic-btn.ptt.pressed {
    color: #6fd08c;
    border-color: rgba(111,208,140,0.7);
    background: rgba(111,208,140,0.16);
    box-shadow: inset 0 2px 4px rgba(0,0,0,0.35);
  }
  .mic-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  /* Web mic enabled but not routed anywhere — muting is meaningless,
     so the button greys out and stops reacting to clicks. Takes
     precedence over `.muted` / `.pressed` via source order below. */
  .mic-btn.idle,
  .mic-btn.idle.muted,
  .mic-btn.idle.ptt.pressed {
    color: var(--muted);
    border-color: var(--border);
    background: rgba(255,255,255,0.02);
    cursor: not-allowed;
    box-shadow: none;
  }
  .mic-btn.idle:hover { background: rgba(255,255,255,0.02); }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    gap: 4px;
    flex: 1;
  }
  .menu-item {
    position: relative;
  }

  /* Simple (childless) top-level items — Mixer, Settings. */
  .menu-item > a {
    display: inline-block;
    padding: 6px 10px;
    color: var(--muted);
    text-decoration: none;
    border-radius: 6px;
    font-size: 14px;
  }
  .menu-item > a:hover { color: var(--text); background: rgba(255,255,255,0.04); }
  .menu-item > a.active {
    color: var(--text);
    background: rgba(122,162,255,0.12);
    box-shadow: inset 0 -2px 0 var(--accent);
  }

  /* Parent pill (Project) is now a single <button> — it only opens the
     dropdown, never navigates. Reset default button chrome and paint the
     pill shape here. Symmetric 10px left/right padding gives the label
     equal breathing room from the pill edges. */
  .parent-row {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    background: transparent;
    color: var(--muted);
    border: 0;
    border-radius: 6px;
    font: inherit;
    font-size: 14px;
    cursor: pointer;
  }
  .parent-row:hover { color: var(--text); background: rgba(255,255,255,0.04); }
  .parent-row.active {
    color: var(--text);
    background: rgba(122,162,255,0.12);
    box-shadow: inset 0 -2px 0 var(--accent);
  }
  .parent-row .label { line-height: 1; }

  /* Caret is a pure visual indicator. Rotation follows isItemOpen() so it
     tracks the true dropdown state (hover-open OR click-open). */
  .caret {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: currentColor;
    transition: transform 0.15s ease;
  }
  .parent-row.open .caret { transform: rotate(180deg); }

  .dropdown {
    display: none;
    position: absolute;
    top: 100%;
    left: 0;
    margin: 4px 0 0 0;
    padding: 4px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: 0 8px 24px rgba(0,0,0,0.35);
    min-width: 160px;
    flex-direction: column;
    gap: 2px;
    z-index: 11;
  }
  .dropdown.visible { display: flex; }
  /* Transparent bridge over the 4px gap between the parent chip and the
     dropdown, so the cursor can travel down without triggering mouseleave
     on the .menu-item. Without it the dropdown snaps shut mid-travel. */
  .dropdown::before {
    content: '';
    position: absolute;
    left: 0;
    right: 0;
    top: -6px;
    height: 6px;
  }
  .dropdown li { width: 100%; }
  .dropdown a {
    display: block;
    padding: 6px 10px;
    color: var(--muted);
    text-decoration: none;
    border-radius: 6px;
    font-size: 14px;
    white-space: nowrap;
  }
  .dropdown a:hover { color: var(--text); background: rgba(255,255,255,0.04); }
  .dropdown a.active {
    color: var(--text);
    background: rgba(122,162,255,0.12);
  }

  .status {
    display: flex;
    align-items: center;
    gap: 14px;
  }
  .project {
    display: flex;
    align-items: center;
    gap: 6px;
    text-decoration: none;
    padding: 4px 8px;
    border-radius: 6px;
    max-width: 240px;
    color: var(--text);
    font-size: 13px;
  }
  .project:hover { background: rgba(255,255,255,0.04); }
  .proj-label {
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    font-size: 10px;
    font-weight: 600;
  }
  .proj-name {
    font-weight: 500;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .proj-none {
    color: var(--muted);
    font-style: italic;
    font-size: 12px;
  }
  .gesture-banner {
    /* Anchor to just below the menubar (which is `position: sticky`) so the
       banner floats over the top of page content instead of squeezing
       between the menu items. `top: 100%` = flush against the menubar's
       bottom edge; `margin-top: 8px` gives it a bit of breathing room. */
    position: absolute;
    top: 100%;
    left: 50%;
    transform: translateX(-50%);
    margin-top: 8px;
    padding: 4px 12px;
    border-radius: 999px;
    background: var(--panel);
    color: var(--accent);
    border: 1px solid var(--accent);
    font-size: 12px;
    font-weight: 500;
    letter-spacing: 0.02em;
    pointer-events: none;
    white-space: nowrap;
    box-shadow: 0 0 12px rgba(122, 162, 255, 0.35);
    animation: gesture-pulse 1.8s ease-in-out infinite;
    z-index: 9;
  }
  @keyframes gesture-pulse {
    0%, 100% { opacity: 0.85; }
    50%      { opacity: 1; }
  }
</style>
