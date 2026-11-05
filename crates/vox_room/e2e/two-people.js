// Two people in one room, each a headless Chromium whose mic is a WAV file:
//   node two-people.js RYAN_LINK ALEX_LINK [SECONDS] [RYAN_WAV] [ALEX_WAV] [SCREENSHOT]
// Prints the AI's state and the conversation as one tab sees it, twice a
// second, and leaves a screenshot behind. Needs `playwright` (with
// Chromium). Each browser is new, so each run wants new links: invite
// links are spent by the Join Audio click, and a second run on the same
// pair gets the expired page.
//
// This is a thing to watch rather than a suite that passes: it wants a
// room with speech to text and something for the AI to answer with, and
// what it is for is seeing two voices become one conversation. The
// transcript filling is itself the proof that the audio arrived -- there
// is nothing to transcribe otherwise -- which is why there is no meter
// here. There used to be one, reading what each tab could hear; the room
// does not show that any more, and `e2e/rejoin.js` is where a microphone
// is checked to be open, from the other side of the room.
//
// Whether the AI answers every pause is the room's own setting: give it
// `--engaged-for-ms`, talk to it in a room of your own, or say its name.
//
//   CHROME=$(which chromium) node two-people.js RYAN_LINK ALEX_LINK
const path = require('path');
const { chromium } = require('playwright');
const { intoARoom } = require('./room.js');
const [ryanUrl, alexUrl, secs = '40', ryanWav = 'ryan.wav', alexWav = 'alex.wav', shot = 'room.png'] = process.argv.slice(2);

async function person(name, url, wav) {
  const browser = await chromium.launch({
    // Playwright's own Chromium doesn't launch everywhere; point CHROME
    // at one that does, as the other scripts here take it.
    executablePath: process.env.CHROME || undefined,
    channel: process.env.CHROME ? undefined : 'chromium',
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
  // Nobody comes in without agreeing to be recorded.
  await page.waitForSelector('.consent input');
  await page.locator('.consent input').check();
  return { name, browser, page };
}

(async () => {
  const ryan = await person('Ryan', ryanUrl, ryanWav);
  const alex = await person('Alex', alexUrl, alexWav);
  const JOIN = 'button:has-text("Join Audio")';
  await Promise.all([ryan.page.click(JOIN), alex.page.click(JOIN)]);
  await ryan.page.waitForTimeout(2000);
  // A tab lands in the lobby, where nothing is written down and there
  // is no AI: both of them walk into the first room of the house, which
  // is the room this is about. Start the room with one.
  console.log(`both into ${await intoARoom(ryan.page)}`);
  await intoARoom(alex.page);
  const t0 = Date.now();
  let last = '';
  while (Date.now() - t0 < Number(secs) * 1000) {
    await new Promise((r) => setTimeout(r, 500));
    // What the AI is up to is on its own strip, not a chip in the top bar.
    const state = await ryan.page.evaluate(() => {
      const vox = [...document.querySelectorAll('.rooms .strip')].pop();
      return vox?.querySelector('.state')?.textContent.trim() ?? '?';
    });
    const log = await alex.page.evaluate(() => [...document.querySelectorAll('.entry')].map((e) =>
      `${e.querySelector('.name').textContent}${e.querySelector('.text').classList.contains('live') ? '*' : ''}: ${e.querySelector('.text').textContent.trim()}`).join(' | '));
    const t = ((Date.now() - t0) / 1000).toFixed(1);
    console.log(`${t.padStart(5)}s ${state}`);
    if (log !== last) { console.log(`        log: ${log}`); last = log; }
  }
  await alex.page.setViewportSize({ width: 1100, height: 720 });
  await alex.page.screenshot({ path: shot });
  console.log('screenshot', shot);
  await ryan.browser.close();
  await alex.browser.close();
})();
