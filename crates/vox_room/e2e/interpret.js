// What one person says, said again for the person who doesn't speak it:
//   node interpret.js RYAN_LINK ALEX_LINK RYAN_WAV ALEX_WAV [SECONDS] [SHOT]
// Two headless Chromiums against a running room that is interpreting.
// Ryan listens in the room's own language and his mic is a WAV of
// somebody talking; Alex listens in the other language the door offers
// and says nothing. This checks the three things interpretation is for:
// that what Ryan said reaches Alex's screen in Alex's language, that
// Alex hears a voice saying it while Ryan's own mic is quiet, and that
// Ryan is shown how far Alex has got through it.
//
// Needs `playwright`, and on NixOS the nix Chromium:
//
//   CHROME=$(which chromium) node interpret.js RYAN_LINK ALEX_LINK en.wav quiet.wav
//
// Each browser is new, so each run wants new invite links.
const path = require('path');
const { chromium } = require('playwright');
const [ryanUrl, alexUrl, ryanWav, alexWav, secs = '90', shot = 'interpret'] =
  process.argv.slice(2);

const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const JOIN = 'button:has-text("Join Audio")';

async function person(name, url, wav, language) {
  const browser = await chromium.launch({
    executablePath: process.env.CHROME || undefined,
    args: [
      '--use-fake-ui-for-media-stream',
      '--use-fake-device-for-media-stream',
      `--use-file-for-fake-audio-capture=${path.resolve(wav)}%noloop`,
      '--autoplay-policy=no-user-gesture-required',
    ],
  });
  const ctx = await browser.newContext({ permissions: ['microphone'] });
  const page = await ctx.newPage();
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

/// The conversation as one tab shows it: who said it, and what that tab
/// is reading, which is not the same text in both tabs.
const log = (page) =>
  page.evaluate(() =>
    [...document.querySelectorAll('.entry')].map((e) => ({
      who: e.querySelector('.name')?.textContent ?? '',
      text: e.querySelector('.text')?.textContent.trim() ?? '',
      tracks: [...e.querySelectorAll('.track')].map((t) => t.textContent.trim()),
    })));

/// What this tab is hearing, off its own meter.
const level = (page) =>
  page.evaluate(() => {
    const s = document.querySelector('.hear span');
    return s ? parseFloat(s.style.width) || 0 : -1;
  });

(async () => {
  const ryan = await person('Ryan', ryanUrl, ryanWav, '');
  const alex = await person('Alex', alexUrl, alexWav, 'Russian');
  await Promise.all([ryan.page.click(JOIN), alex.page.click(JOIN)]);

  // The conversation may already have lines in it from before this run;
  // the log only ever grows, so anything past this mark is ours.
  await wait(1500);
  const mark = (await log(alex.page)).length;
  console.log(`watching from line ${mark}`);

  const t0 = Date.now();
  let translated = null;
  let row = null;
  let heardOut = null;
  let shotTaken = false;
  const rows = new Map();
  let alexHeard = 0;
  let last = '';
  while (Date.now() - t0 < Number(secs) * 1000) {
    await wait(500);
    const [mine, theirs, lvl] = await Promise.all([
      log(ryan.page),
      log(alex.page),
      level(alex.page),
    ]);
    alexHeard = Math.max(alexHeard, lvl);
    const fresh = theirs.slice(mark).filter((e) => e.who === 'Ryan');
    const line = fresh.map((e) => e.text).join(' | ');
    if (line && line !== last) {
      console.log(`${((Date.now() - t0) / 1000).toFixed(1).padStart(5)}s  alex reads: ${line}`);
      last = line;
    }
    // Alex is reading Ryan in Cyrillic: Ryan typed nothing and speaks
    // no Russian, so that text can only be the interpreter's.
    const ru = fresh.find((e) => /[\u0400-\u04ff]/.test(e.text));
    if (ru && !translated) {
      translated = ru.text;
      await alex.page.screenshot({ path: `${shot}-alex.png` });
    }
    // And Ryan is shown how far Alex has got through it. The row goes
    // when Alex has heard it out, which is the thing it is there to
    // say, so a line that had one and then hasn't is one that landed.
    mine.slice(mark).forEach((e, n) => {
      if (e.who !== 'Ryan') return;
      if (e.tracks.length) {
        rows.set(n, e.tracks);
        if (!row) row = e.tracks;
      } else if (rows.has(n) && !heardOut) {
        heardOut = rows.get(n);
      }
    });
    if (row && !shotTaken) {
      shotTaken = true;
      await ryan.page.screenshot({ path: `${shot}-ryan.png` });
    }
    if (translated && row && heardOut && alexHeard > 5) break;
  }

  const problems = [];
  if (!translated) problems.push("nothing of Ryan's reached Alex in Russian");
  if (!row) problems.push('Ryan was never shown how far Alex had got');
  if (!heardOut) problems.push('the row never went, so Alex was never shown to have heard it out');
  if (alexHeard <= 5) problems.push(`Alex heard nothing (meter peaked at ${alexHeard}%)`);
  console.log(`\nAlex read: ${translated ?? '(nothing)'}`);
  console.log(`Ryan saw:  ${row?.join('   ') ?? '(no row)'}`);
  console.log(`last seen: ${heardOut?.join('   ') ?? '(the row never went)'}`);
  console.log(`Alex's meter peaked at ${alexHeard}%`);
  console.log(problems.length ? `FAILED: ${problems.join('; ')}` : 'OK: it was said again, and heard');
  console.log('screenshots', `${shot}-alex.png`, `${shot}-ryan.png`);
  await ryan.browser.close();
  await alex.browser.close();
  process.exit(problems.length ? 1 : 0);
})();
