// Two people in the room, each a headless Chromium whose mic is a WAV file:
//   node two-people.js RYAN_LINK ALEX_LINK [SECONDS] [RYAN_WAV] [ALEX_WAV] [SCREENSHOT]
// Prints, twice a second, how loud each tab's mix is (each should hear
// the other and the AI, never themselves), the AI's state, and the
// conversation as one tab sees it. Needs `playwright` (with Chromium).
const path = require('path');
const { chromium } = require('playwright');
const [ryanUrl, alexUrl, secs = '40', ryanWav = 'ryan.wav', alexWav = 'alex.wav', shot = 'room.png'] = process.argv.slice(2);

async function person(name, url, wav) {
  const browser = await chromium.launch({
    args: [
      '--use-fake-ui-for-media-stream',
      '--use-fake-device-for-media-stream',
      `--use-file-for-fake-audio-capture=${path.resolve(wav)}%noloop`,
      '--autoplay-policy=no-user-gesture-required',
    ],
  });
  const ctx = await browser.newContext({ permissions: ['microphone'] });
  const page = await ctx.newPage();
  page.on('console', (m) => { if (m.type() === 'error' || m.type() === 'warning') console.log(`[${name} console] ${m.text()}`); });
  page.on('pageerror', (e) => console.log(`[${name} pageerror] ${e}`));
  await page.goto(url);
  await page.waitForSelector('text=Join audio');
  return { name, browser, page };
}

(async () => {
  const ryan = await person('Ryan', ryanUrl, ryanWav);
  const alex = await person('Alex', alexUrl, alexWav);
  await ryan.page.click('text=After every pause');
  await Promise.all([ryan.page.click('text=Join audio'), alex.page.click('text=Join audio')]);
  const t0 = Date.now();
  // Expose what each tab hears (peak since last read) through the meter width.
  const heard = (p) => p.evaluate(() => {
    const s = document.querySelector('.hear span');
    return s ? parseFloat(s.style.width) || 0 : -1;
  });
  let last = '';
  while (Date.now() - t0 < Number(secs) * 1000) {
    await new Promise((r) => setTimeout(r, 500));
    const [hr, ha] = await Promise.all([heard(ryan.page), heard(alex.page)]);
    const state = await ryan.page.evaluate(() => document.querySelector('.pill')?.textContent);
    const log = await alex.page.evaluate(() => [...document.querySelectorAll('.entry')].map((e) =>
      `${e.querySelector('.name').textContent}${e.querySelector('.text').classList.contains('live') ? '*' : ''}: ${e.querySelector('.text').textContent.trim()}`).join(' | '));
    const t = ((Date.now() - t0) / 1000).toFixed(1);
    console.log(`${t.padStart(5)}s ryan hears ${String(hr.toFixed(0)).padStart(3)}% alex hears ${String(ha.toFixed(0)).padStart(3)}% | ${state}`);
    if (log !== last) { console.log(`        log: ${log}`); last = log; }
  }
  await alex.page.setViewportSize({ width: 1100, height: 720 });
  await alex.page.screenshot({ path: shot });
  console.log('screenshot', shot);
  await ryan.browser.close();
  await alex.browser.close();
})();
