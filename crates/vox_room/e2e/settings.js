// What is along the foot of the room, and what the cog opens:
//   node settings.js RYAN_LINK
//
// The footer is what you reach for while talking -- your mic on the
// left, what you do with the room on the right -- and the one button
// there is the same width whatever it says, so it doesn't change shape
// under your hand. Everything you set once is on a page of its own
// behind the cog, with the room still running: still in the audio, and
// the space bar still yours.
//
//   vox_room --room Lobby --room Studio --echo --no-stt --tts none

const { is, report, person, wait } = require('./room.js');
const [ryanLink] = process.argv.slice(2);

/// Every button along the foot, in order, with where it sits.
const feet = (page) =>
  page.evaluate(() =>
    [...document.querySelectorAll('footer button')].map((b) => ({
      what: (b.getAttribute('aria-label') ?? b.textContent).replace(/\s+/g, ' ').trim(),
      width: Math.round(b.getBoundingClientRect().width),
      left: Math.round(b.getBoundingClientRect().left),
    })),
  );

(async () => {
  const { browser, page } = await person(ryanLink);
  await page.setViewportSize({ width: 1280, height: 860 });
  await wait(400);

  // Mute on the left; the room's own buttons over on the right.
  const row = await feet(page);
  is('what is down there', row.map((b) => b.what), ['Mute Space', 'Share screen', 'Leave audio', 'Settings']);
  const foot = await page.evaluate(() => document.querySelector('footer').getBoundingClientRect().width);
  is('mute is on the left', row[0].left < foot / 3, true);
  is('and the rest are over on the other side', row[1].left > foot / 2, true);
  is('with the cog last, after leaving', row[3].left > row[2].left, true);

  // The same button whatever it says: Unmute is no wider than Mute.
  const wide = row[0].width;
  await page.click('footer button.big');
  await wait(400);
  const muted = await feet(page);
  is('it says the other thing now', muted[0].what, 'Unmute Space');
  is('and is the same width', muted[0].width, wide);
  is('so nothing beside it moved', muted[1].left, row[1].left);
  await page.click('footer button.big');
  await wait(400);

  // Nothing is strung along it but the one switch that says what the
  // button beside it does.
  is('no sliders or readouts down there',
    await page.evaluate(() => ({
      sliders: document.querySelectorAll('footer input[type=range]').length,
      boxes: document.querySelectorAll('footer input[type=checkbox]').length,
      meters: document.querySelectorAll('footer .hear').length,
    })),
    { sliders: 0, boxes: 1, meters: 0 });

  // The cog opens a page, and the room is not behind it.
  await page.click('footer button.cog');
  await page.waitForSelector('.settings');
  is('the settings are a page of their own',
    await page.evaluate(() => ({
      settings: !!document.querySelector('.settings'),
      room: !!document.querySelector('.app'),
      log: !!document.querySelector('.log'),
    })),
    { settings: true, room: false, log: false });
  // What it is for: the things you set once and don't watch. Push to
  // talk is not one of those -- it says what the button under your
  // hand does -- so it is beside that button and not here.
  is('what is set here',
    await page.evaluate(() => ({
      name: !!document.querySelector('.settings .name input'),
      volume: !!document.querySelector('.settings input[type=range]'),
      pushToTalk: !!document.querySelector('.settings input[type=checkbox]'),
      meter: !!document.querySelector('.settings .hear'),
    })),
    { name: true, volume: true, pushToTalk: false, meter: false });

  // The room kept running: still joined, and the socket never went.
  is('still in the audio', await page.evaluate(() => !document.querySelector('.enter')), true);
  await page.click('.settings button.primary');
  await wait(500);
  is('back in the room', await page.evaluate(() => !!document.querySelector('.log')), true);

  // Push to talk, beside the button it changes: ticking it is what
  // turns that button into one you hold.
  await page.click('footer .switch input');
  await wait(400);
  is('the button says to hold it', (await feet(page))[0].what, 'Hold to talk Space');
  is('and is the same width', (await feet(page))[0].width, wide);
  await page.click('footer .switch input');
  await wait(400);
  is('and back again', (await feet(page))[0].what, 'Mute Space');

  // Escape closes it too, and the cog says whether it is open.
  await page.click('footer button.cog');
  await page.waitForSelector('.settings');
  await page.keyboard.press('Escape');
  await wait(500);
  is('Escape closes it', await page.evaluate(() => !!document.querySelector('.settings')), false);
  is('and the cog is not pressed',
    await page.evaluate(() => document.querySelector('footer button.cog').getAttribute('aria-pressed')),
    'false');

  // And the way out is still the way out: leaving from the room puts
  // you at the door, and coming back lands in the conversation rather
  // than in the settings somebody left open.
  await page.click('footer button.leave');
  await page.waitForSelector('.enter');
  await page.locator('.consent input').check();
  await page.click('button:has-text("Join Audio")');
  await page.waitForTimeout(2600);
  is('coming back lands in the room',
    await page.evaluate(() => !!document.querySelector('.log') && !document.querySelector('.settings')),
    true);

  await browser.close();
  report();
})();
