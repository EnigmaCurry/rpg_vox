/// Which look the room is wearing.
///
/// A theme is a block of custom properties in `src/themes/*.css` keyed
/// on `data-theme` on the <html> element, and this is the whole of the
/// machinery around them: the list of them, which one is on, and
/// remembering it.
///
/// It is this browser's choice and nobody else's. Nothing about a theme
/// goes to the server: two people in the same room can be looking at it
/// in two different ones, and neither can tell. That is also why it is
/// in `localStorage` rather than in the room's own settings -- it is a
/// fact about a screen, not about a person, and the same person at a
/// desk and on a phone may well want different answers.
///
/// Setting it before the page is drawn is `index.html`'s job: a theme
/// applied from here would land after the first paint, and the room
/// would flash white on its way to being black. The script there and
/// `KEY` below are the same string, deliberately; see the note on it.

/// Every theme there is, in the order the picker shows them.
export const THEMES = [
  {
    id: 'obsidian',
    name: 'Obsidian Comms',
    blurb: 'Dark glass and cut corners. The AI speaks in violet, you in amber.',
  },
  {
    id: 'legacy',
    name: 'Plain',
    blurb: 'Paper by day, graphite by night — whichever your machine is set to.',
  },
];

/// What a browser that has never chosen gets.
export const FALLBACK = 'obsidian';

/// Where the choice is kept. `index.html` reads this same key in a
/// script that runs before anything is drawn, so the two must agree;
/// if this ever changes, change it there in the same commit.
const KEY = 'vox_room.theme';

function stored() {
  try {
    const id = localStorage.getItem(KEY);
    return THEMES.some((t) => t.id === id) ? id : FALLBACK;
  } catch {
    return FALLBACK;
  }
}

/// Which one is on. Read it to draw the picker; `setTheme` changes it.
export const theme = $state({ id: stored() });

/// Wear a theme, and keep wearing it next time.
///
/// A name the room doesn't know is the default rather than an error: a
/// theme can be taken out of the list, and a browser still carrying its
/// name should come up looking like something.
export function setTheme(id) {
  const t = THEMES.find((t) => t.id === id) ?? THEMES.find((t) => t.id === FALLBACK);
  theme.id = t.id;
  document.documentElement.dataset.theme = t.id;
  try {
    localStorage.setItem(KEY, t.id);
  } catch {}
}

/// Put back on whatever was being worn. Called once, as the app starts,
/// so that a browser whose `localStorage` the inline script could not
/// read -- or one whose stored theme has since been removed -- still
/// ends up with the attribute set to something real.
export function wearTheme() {
  setTheme(theme.id);
}
