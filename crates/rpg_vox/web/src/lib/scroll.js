/// Scroll a clip element into view IF it's currently offscreen inside its
/// nearest scrollable ancestor. Idempotent when already visible so the
/// timeline doesn't twitch when the playing clip is on-screen.
///
/// "Pages at a time": rather than nudging the clip just barely into view,
/// scroll far enough that the clip sits near the TOP of the container.
/// That reveals the following page of upcoming clips so the user sees
/// where playback is heading, and repeated scrolls between clips don't
/// keep re-triggering as long as the next few clips stay in view.
export function scrollClipIntoView(el) {
  if (!el || !el.getBoundingClientRect) return;
  const parent = findScrollableAncestor(el);
  if (!parent) return;
  const rect = el.getBoundingClientRect();
  const parentRect = parent.getBoundingClientRect();
  // Small padding at the edges so a partially-cut clip is still treated
  // as visible enough to skip scrolling.
  const PAD = 12;
  const visibleTop = rect.top >= parentRect.top - PAD;
  const visibleBottom = rect.bottom <= parentRect.bottom + PAD;
  if (visibleTop && visibleBottom) return;
  // Target: put the clip near the top of the container, leaving the rest
  // of the container to show upcoming clips ("page-at-a-time" reveal).
  const topWithinScroll = rect.top - parentRect.top + parent.scrollTop;
  parent.scrollTo({
    top: Math.max(0, topWithinScroll - 24),
    behavior: 'smooth',
  });
}

/// First ancestor whose computed overflowY allows scrolling. Falls back
/// to the document element, which handles the "page itself scrolls" case
/// (e.g. Script.svelte's full-viewport layout has an explicit scroll
/// container, but user-turn play buttons live one level up from that).
function findScrollableAncestor(el) {
  let p = el.parentElement;
  while (p) {
    const style = getComputedStyle(p);
    if (/(auto|scroll|overlay)/.test(style.overflowY) && p.scrollHeight > p.clientHeight) {
      return p;
    }
    p = p.parentElement;
  }
  return document.scrollingElement || document.documentElement;
}
