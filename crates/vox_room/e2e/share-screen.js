// One person shares a desktop, another watches it:
//   node share-screen.js RYAN_LINK ALEX_LINK [SCREENSHOT_PREFIX]
// Two headless Chromiums against a running room. Ryan shares, and this
// checks what the pictures actually are: that Alex's mixer fills in with
// a thumbnail of Ryan's desktop, that Ryan's own strip shows the picture
// he is encoding, that opening it full size is Alex's alone -- Ryan's
// screen doesn't change -- and that closing it stops the full-size
// stream -- and that Alex walking out and back in finds the desktop
// still there. Needs `playwright` (with Chromium). Each browser is new, so
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
const { goTo, intoARoom } = require('./room.js');
const [ryanUrl, alexUrl, prefix = 'share'] = process.argv.slice(2);

const wait = (ms) => new Promise((r) => setTimeout(r, ms));

// The button, not the card's prose about it.
const JOIN = 'button:has-text("Join Audio")';
// Nobody comes in without agreeing to be recorded, so the box is part of
// pressing the button now.
const AGREE = '.consent input';
const SHARE = '.tools button.share';
const STOP = '.tools button.share.on';

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
    // Every socket the page opens, so a test can cut one the way a
    // dropped connection would.
    const Real = window.WebSocket;
    window.__sockets = [];
    window.WebSocket = class extends Real {
      constructor(...a) {
        super(...a);
        window.__sockets.push(this);
      }
    };
  });
  const page = await ctx.newPage();
  await page.setViewportSize({ width: 1200, height: 760 });
  page.on('console', (m) => {
    if (m.type() === 'error' || m.type() === 'warning') console.log(`[${name} console] ${m.text()}`);
  });
  page.on('pageerror', (e) => console.log(`[${name} pageerror] ${e}`));
  await page.goto(url);
  await page.waitForSelector(JOIN);
  await page.locator(AGREE).check();
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
  // Out of the lobby and into the first room of the house: a desktop
  // is shared to the room you are in, and the lobby is the room nobody
  // stays in. Start the room with one.
  const here = await intoARoom(ryan.page);
  await intoARoom(alex.page);
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

  // Who is sharing comes down the desktop socket and the change of
  // room down the other, in no particular order. A tab that let the
  // room's `hello` wipe a list that was already the new room's saw
  // nobody sharing until somebody started or stopped. Racy by nature,
  // so it goes round a few times.
  console.log('\nAlex walks out and back in');
  for (let i = 1; i <= 4; i++) {
    await goTo(alex.page, 'Studio', 600);
    await goTo(alex.page, here, 0);
    const back = await painted(alex.page, thumb, 8000);
    check(back?.spread > 20, `Ryan's desktop is still in Alex's mixer when he is back (${i})`, JSON.stringify(back));
  }

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

  // Ryan's connection drops with Alex watching. The picture stays up
  // waiting, and when Ryan's tab is back sharing -- a new stream, by
  // the server's count -- Alex is looking at it again without doing
  // anything.
  console.log("\nRyan's desktop socket drops while Alex watches");
  await ryan.page.evaluate(() => {
    for (const s of window.__sockets) if (s.url.endsWith('/screen.ws')) s.close();
  });
  await alex.page.waitForSelector('.board .lost', { timeout: 5000 }).catch(() => {});
  check(!!(await alex.page.locator('.board .lost').count()), 'Alex is told Ryan dropped off, and the view stays up');
  await alex.page.screenshot({ path: `${prefix}-waiting.png` });
  await alex.page.waitForSelector('.board .lost', { state: 'detached', timeout: 15000 }).catch(() => {});
  check(!(await alex.page.locator('.board .lost').count()), 'and stops waiting when Ryan is back');
  const again = await painted(alex.page, '.board canvas');
  check(again?.spread > 20 && again.w > alexThumb.w, "Alex is watching Ryan's desktop full size again", JSON.stringify(again));

  console.log('\nAlex closes it');
  await alex.page.click('.board button');
  await alex.page.waitForSelector('.board', { state: 'detached' });
  // The thumbnail carries on: it is everyone's, all the time.
  const after = await painted(alex.page, thumb);
  check(after?.spread > 20, 'the thumbnail carries on after the full size is closed', JSON.stringify(after));

  console.log('\nRyan stops sharing, with Alex watching');
  await alex.page.click(`${strip('Ryan')} .desktop`);
  await alex.page.waitForSelector('.board');
  await ryan.page.click(STOP);
  // On purpose is not a drop: nothing to wait for.
  await alex.page.waitForSelector('.board', { state: 'detached', timeout: 5000 }).catch(() => {});
  check(!(await alex.page.locator('.board').count()), "Alex's full-size view closes at once");
  await alex.page.waitForSelector(`${strip('Ryan')} .desktop`, { state: 'detached' });
  check(true, "the thumbnail goes from Alex's mixer");

  await alex.page.screenshot({ path: `${prefix}-mixer.png` });
  console.log(`\nscreenshots: ${prefix}-watching.png, ${prefix}-mixer.png`);
  await ryan.browser.close();
  await alex.browser.close();
  console.log(failures ? `\n${failures} failed` : '\nall good');
  process.exit(failures ? 1 : 0);
})();
