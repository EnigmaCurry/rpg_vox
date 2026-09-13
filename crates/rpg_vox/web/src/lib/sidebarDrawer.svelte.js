// Shared mobile-drawer state for the left-rail sidebars (Recordings,
// Scenes, Scripts). Under 1080px each sidebar becomes a slide-in drawer
// controlled by a hamburger button that stays fixed below the menubar.
// Above 1080px the sidebar sits in the layout normally and the drawer
// state is ignored.
//
// Each sidebar owns its own drawer instance so opening the Scripts
// drawer while on the Scripts page doesn't affect the Recordings
// drawer's state on the Record page. The instance exposes:
//   * `isMobile` — reactive, tracks the (max-width: 1079px) media query
//   * `open` / `setOpen` / `toggle` / `close` — drawer visibility
//   * `wrap(fn)` — helper that returns a wrapped callback which closes
//     the drawer after invoking `fn` (mobile only). Use this on
//     selection handlers so tapping a row in the drawer dismisses it.
//
// Escape closes the drawer. The consumer supplies the click-outside
// backdrop as a normal element (the shared CSS class does the heavy
// lifting); the module doesn't attach global click listeners.

import { onMount, onDestroy } from 'svelte';

export const DRAWER_BREAKPOINT_PX = 1080;

export function createSidebarDrawer() {
  let isMobile = $state(false);
  let open = $state(false);

  let mql = null;
  const onMqChange = () => { isMobile = mql?.matches ?? false; };
  const onKey = (e) => {
    if (e.key === 'Escape' && open) { e.preventDefault(); open = false; }
  };

  onMount(() => {
    mql = window.matchMedia(`(max-width: ${DRAWER_BREAKPOINT_PX - 1}px)`);
    isMobile = mql.matches;
    mql.addEventListener('change', onMqChange);
    window.addEventListener('keydown', onKey);
  });
  onDestroy(() => {
    mql?.removeEventListener('change', onMqChange);
    window.removeEventListener('keydown', onKey);
  });

  return {
    get isMobile() { return isMobile; },
    get open() { return open; },
    setOpen(v) { open = !!v; },
    toggle() { open = !open; },
    close() { open = false; },
    // Wrap a selection handler so tapping a drawer item also closes the
    // drawer. Above the breakpoint this is a no-op passthrough.
    wrap(fn) {
      return (...args) => {
        const r = fn?.(...args);
        if (isMobile) open = false;
        return r;
      };
    },
  };
}
