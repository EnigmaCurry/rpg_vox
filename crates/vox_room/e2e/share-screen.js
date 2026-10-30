// One person shares a desktop, another watches it:
//   node share-screen.js RYAN_LINK ALEX_LINK [SCREENSHOT_PREFIX]
// Two headless Chromiums against a running room. Ryan shares, and this
// checks what the pictures actually are: that Alex's mixer fills in with
// a thumbnail of Ryan's desktop, that Ryan's own strip shows the picture
// he is encoding, that opening it full size is Alex's alone -- Ryan's
// screen doesn't change -- and that closing it stops the full-size
// stream. Needs `playwright` (with Chromium). Each browser is new, so
// each run wants new invite links.
//
// Headless Chromium has no desktop to capture, so the script makes
// getDisplayMedia capture the tab itself (`preferCurrentTab`, with
// --auto-accept-this-tab-capture). The room's own page is what gets
// shared, which is colourful enough to tell from a blank canvas. That
// needs a whole Chromium rather than playwright's headless shell, which
// has no capture in it: set CHROME to one if playwright hasn't got it.
//
//   CHROME=$(which chromium) node share-screen.js RYAN_LINK ALEX_LINK
const { chromium } = require('playwright');
const [ryanUrl, alexUrl, prefix = 'share'] = process.argv.slice(2);

const wait = (ms) => new Promise((r) => setTimeout(r, ms));

// The button, not the card's prose about it.
const JOIN = 'button:has-text("Join Audio")';
const SHARE = 'footer button:has-text("Share screen")';
const STOP = 'footer button:has-text("Stop sharing")';

async function person(name, url) {
  const browser = await chromium.launch({
    executablePath: process.env.CHROME || undefined,
    channel: process.env.CHROME ? undefined : 'chromium',
    args: [
      '--use-fake-ui-for-media-stream',
      '--use-fake-device-for-media-stream',
      '--auto-accept-this-tab-capture',
      '--autoplay-policy=no-user-gesture-required',
    ],
  });
  const ctx = await browser.newContext({ permissions: ['microphone'] });
  await ctx.addInitScript(() => {
    const real = navigator.mediaDevices.getDisplayMedia.bind(navigator.mediaDevices);
    navigator.mediaDevices.getDisplayMedia = (opts) => real({ ...opts, preferCurrentTab: true });
  });
  const page = await ctx.newPage();
  await page.setViewportSize({ width: 1200, height: 760 });
  page.on('console', (m) => {
    if (m.type() === 'error' || m.type() === 'warning') console.log(`[${name} console] ${m.text()}`);
  });
  page.on('pageerror', (e) => console.log(`[${name} pageerror] ${e}`));
  await page.goto(url);
  await page.waitForSelector(JOIN);
  return { name, browser, page };
}

/// What is actually on a canvas: its size, and how much the picture
/// varies. A canvas nothing has painted is flat (`spread` 0). Resolved
/// through a locator, because the selectors here are playwright's and
/// not the browser's.
async function look(page, selector) {
  const canvas = page.locator(selector).first();
  if (!(await canvas.count())) return null;
  return canvas.evaluate((c) => {
    if (!c.width || !c.height) return { w: c.width, h: c.height, spread: 0 };
    const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
    let min = 255;
    let max = 0;
    for (let i = 0; i < d.length; i += 4) {
      min = Math.min(min, d[i]);
      max = Math.max(max, d[i]);
    }
    return { w: c.width, h: c.height, spread: max - min };
  });
}

/// The strip in the mixer for one person, by the name on it.
function strip(person) {
  return `.strip:has(.label:text-is("${person}"))`;
}

let failures = 0;
function check(ok, what, detail = '') {
  console.log(`${ok ? '  ok  ' : ' FAIL '} ${what}${detail ? ` -- ${detail}` : ''}`);
  if (!ok) failures++;
}

/// Wait for a canvas to have a picture on it, up to `ms`.
async function painted(page, selector, ms = 15000) {
  const until = Date.now() + ms;
  let last = null;
  while (Date.now() < until) {
    last = await look(page, selector);
    if (last && last.spread > 20) return last;
    await wait(250);
  }
  return last;
}

(async () => {
  const ryan = await person('Ryan', ryanUrl);
  const alex = await person('Alex', alexUrl);
  await Promise.all([
    ryan.page.click(JOIN),
    alex.page.click(JOIN),
  ]);
  await Promise.all([
    ryan.page.waitForSelector(strip('Alex')),
    alex.page.waitForSelector(strip('Ryan')),
  ]);
  console.log('both in the room');

  console.log('\nRyan shares his desktop');
  await ryan.page.click(SHARE);
  await ryan.page.waitForSelector(STOP);

  const thumb = `${strip('Ryan')} .desktop canvas`;
  const alexThumb = await painted(alex.page, thumb);
  check(alexThumb?.spread > 20, "Alex's mixer shows a thumbnail of Ryan's desktop", JSON.stringify(alexThumb));
  const ownThumb = await painted(ryan.page, thumb);
  check(ownThumb?.spread > 20, 'Ryan sees the picture he is encoding, in his own strip', JSON.stringify(ownThumb));
  check(
    !(await alex.page.locator(`${strip('Alex')} .desktop`).count()),
    "Alex, who is sharing nothing, has no thumbnail of his own",
  );

  console.log('\nAlex opens it full size');
  await alex.page.click(`${strip('Ryan')} .desktop`);
  await alex.page.waitForSelector('.board');
  const board = await painted(alex.page, '.board canvas');
  check(board?.spread > 20, 'the full size fills in for Alex', JSON.stringify(board));
  check(
    board && alexThumb && board.w > alexThumb.w,
    'and it is the bigger picture, not the thumbnail',
    `${board?.w} > ${alexThumb?.w}`,
  );
  check(!(await ryan.page.locator('.board').count()), "Ryan's screen did not change: watching is not shared");
  check(
    !!(await alex.page.locator(`${strip('Ryan')} .desktop canvas`).count()),
    'the thumbnail is still there beside it',
  );

  await alex.page.screenshot({ path: `${prefix}-watching.png` });

  console.log('\nAlex closes it');
  await alex.page.click('.board button');
  await alex.page.waitForSelector('.board', { state: 'detached' });
  // The thumbnail carries on: it is everyone's, all the time.
  const after = await painted(alex.page, thumb);
  check(after?.spread > 20, 'the thumbnail carries on after the full size is closed', JSON.stringify(after));

  console.log('\nRyan stops sharing');
  await ryan.page.click(STOP);
  await alex.page.waitForSelector(`${strip('Ryan')} .desktop`, { state: 'detached' });
  check(true, "the thumbnail goes from Alex's mixer");

  await alex.page.screenshot({ path: `${prefix}-mixer.png` });
  console.log(`\nscreenshots: ${prefix}-watching.png, ${prefix}-mixer.png`);
  await ryan.browser.close();
  await alex.browser.close();
  console.log(failures ? `\n${failures} failed` : '\nall good');
  process.exit(failures ? 1 : 0);
})();
