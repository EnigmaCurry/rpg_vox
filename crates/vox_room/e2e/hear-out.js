// Two people hearing the same reply in different languages:
//   node hear-out.js RYAN_LINK ALEX_LINK [SECONDS] [SCREENSHOT]
// Ryan listens in the room's own language, Alex in the other one the
// door offers, and Ryan types a question (no mic needed, and nothing to
// transcribe). The same reply takes a different time to say in each
// language, so each tab shows how far it has been heard in both; this
// reads that row off both screens and checks the three things about it
// that matter: both tabs are told the same thing, one language reaches
// `heard` while the other is still going, and the row goes when the
// turn is over.
//
// Needs `playwright`, and on NixOS the nix Chromium rather than
// playwright's own:
//
//   CHROME=$(which chromium) node hear-out.js RYAN_LINK ALEX_LINK
//
// Each browser is new, so each run wants new invite links: the Join
// Audio click spends them.
const { chromium } = require('playwright');
const [ryanUrl, alexUrl, secs = '60', shot = 'hear-out.png'] = process.argv.slice(2);

const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const JOIN = 'button:has-text("Join Audio")';

async function person(name, url, language) {
  const browser = await chromium.launch({
    executablePath: process.env.CHROME || undefined,
    args: [
      '--use-fake-ui-for-media-stream',
      '--use-fake-device-for-media-stream',
      '--autoplay-policy=no-user-gesture-required',
    ],
  });
  const ctx = await browser.newContext({ permissions: ['microphone'] });
  const page = await ctx.newPage();
  // Sized before anything is read, not at the screenshot: resizing moves
  // the log's scroll without the page's own "follow the new text" effect
  // running again, and the shot comes out scrolled off the bottom.
  await page.setViewportSize({ width: 1100, height: 760 });
  page.on('console', (m) => {
    if (m.type() === 'error') console.log(`[${name} console] ${m.text()}`);
  });
  page.on('pageerror', (e) => console.log(`[${name} pageerror] ${e}`));
  await page.goto(url);
  await page.waitForSelector(JOIN);
  if (language) await page.selectOption('label:has-text("Language") select', language);
  return { name, browser, page };
}

/// The per-language row under the AI's live reply, as one tab shows it.
const tracks = (page) =>
  page.evaluate(() =>
    [...document.querySelectorAll('.entry.agent .tracks')].flatMap((row) =>
      [...row.querySelectorAll('.track')].map((t) => ({
        language: t.querySelector('.lang').textContent,
        left: t.querySelector('.left').textContent,
        width: t.querySelector('.bar i').style.width,
        done: t.classList.contains('done'),
      }))));

const show = (ts) =>
  ts.map((t) => `${t.language} ${t.left}${t.done ? '✓' : ''} ${t.width}`).join('   ') || '(no row)';

(async () => {
  const ryan = await person('Ryan', ryanUrl, '');
  const alex = await person('Alex', alexUrl, 'Russian');
  await Promise.all([ryan.page.click(JOIN), alex.page.click(JOIN)]);
  await wait(1000);
  // Named, which is the room's own default: the message says the name,
  // so there is no mode to set and no microphone in this at all.
  await ryan.page.fill('.composer input', 'Vox, say two sentences about the sea.');
  await ryan.page.click('.composer button');

  const t0 = Date.now();
  let last = '';
  // The one thing this is for: a language heard out while another is
  // still going. Both tabs must see it -- it is nobody's private news.
  const split = { Ryan: null, Alex: null };
  let differed = null;
  let wentAway = false;
  for (const p of [ryan, alex]) p.shot = shot.replace(/(\.png)?$/, `-${p.name.toLowerCase()}.png`);
  while (Date.now() - t0 < Number(secs) * 1000) {
    await wait(200);
    const rows = await Promise.all([tracks(ryan.page), tracks(alex.page)]);
    const line = show(rows[0]);
    if (line !== last) {
      console.log(`${((Date.now() - t0) / 1000).toFixed(1).padStart(5)}s  ${line}`);
      last = line;
    }
    // Only the languages and their order: the two tabs are read one
    // after the other, so a fifth of a second of playback can fall
    // between them and the numbers are allowed to differ by that.
    const langs = rows.map((r) => r.map((t) => t.language).join(','));
    if (rows.every((r) => r.length) && langs[0] !== langs[1]) differed = langs.join(' vs ');
    for (const [i, p] of [ryan, alex].entries()) {
      if (!split[p.name] && rows[i].some((t) => t.done) && rows[i].some((t) => !t.done)) {
        split[p.name] = rows[i];
        await p.page.screenshot({ path: p.shot });
      }
    }
    // The row goes when there is nothing left to wait for, which is the
    // other half of saying so.
    if (split.Ryan && split.Alex && !rows[0].length && !rows[1].length) {
      wentAway = true;
      break;
    }
  }

  const problems = [];
  for (const [who, r] of Object.entries(split)) {
    if (!r) problems.push(`${who} never saw one language heard out while another was still going`);
  }
  if (differed) problems.push(`the tabs were shown different languages (${differed})`);
  if (!wentAway) problems.push('the row never went away');
  const seen = split.Ryan ?? split.Alex;
  if (seen) {
    const done = (b) => seen.filter((t) => t.done === b).map((t) => t.language).join(', ');
    console.log(`\nheard out first: ${done(true)}; still going: ${done(false)}`);
  }
  console.log(problems.length ? `FAILED: ${problems.join('; ')}` : 'OK: both tabs could see it out');
  console.log('screenshots', ryan.shot, alex.shot);
  await ryan.browser.close();
  await alex.browser.close();
  process.exit(problems.length ? 1 : 0);
})();
