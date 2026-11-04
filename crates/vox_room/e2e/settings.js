// What is along the foot of the room, and what the cog opens:
//   node settings.js RYAN_LINK
//
// The footer is the two ways of saying something and nothing else:
// the button you talk with -- the same width whatever it says, so it
// doesn't change shape under your hand -- and the box you type in.
// What you do with the room is over the people in it, as icons.
// Everything you set once is on a page of its own behind the cog, with
// the room still running: still in the audio, and the space bar still
// yours.
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

/// The two edges of the box you type in: where it starts, which is what
/// the talk button must not move when it changes what it says, and
/// where it ends, which is the end of the footer.
const sayAt = (page) => page.evaluate(() => {
  const r = document.querySelector('.say').getBoundingClientRect();
  return { left: Math.round(r.left), right: Math.round(r.right) };
});

(async () => {
  const { browser, page } = await person(ryanLink);
  await page.setViewportSize({ width: 1280, height: 860 });
  await wait(400);

  // The button you talk with, the two switches that say what the room
  // does with your microphone and with the AIs, and the box you type
  // in. Nothing else -- and Send is not down there at all until there
  // is something to send.
  const row = await feet(page);
  const wide = row[0].width;
  is('what is down there', row.map((b) => b.what), ['Mute Space', 'PTT', 'BOTS']);
  const foot = await page.evaluate(() => document.querySelector('footer').getBoundingClientRect().width);
  is('mute is on the left', row[0].left < foot / 3, true);
  const box = await sayAt(page);
  is('and the box you type in takes the rest',
    box.left > row[0].left + wide && foot - box.right < 20, true);

  // The two switches are one column beside that button, each half its
  // height, so the pair is exactly as tall as the button they are
  // about and neither of them can move it.
  is('the switches are stacked beside it, half-height each',
    await page.evaluate(() => {
      const big = document.querySelector('footer button.big').getBoundingClientRect();
      const ptt = document.querySelector('footer .ptt-switch').getBoundingClientRect();
      const bots = document.querySelector('footer .bots-switch').getBoundingClientRect();
      return {
        beside: Math.round(ptt.left) === Math.round(bots.left) && ptt.left > big.right - 1,
        stacked: bots.top > ptt.top,
        half: ptt.height < big.height * 0.6 && bots.height < big.height * 0.6,
        together: Math.abs((bots.bottom - ptt.top) - big.height) < 8,
      };
    }),
    { beside: true, stacked: true, half: true, together: true });

  // Send turns up with the first thing worth sending, and goes with it.
  await page.fill('.say textarea', 'something');
  await wait(200);
  is('send arrives with something to send', (await feet(page)).map((b) => b.what),
    ['Mute Space', 'PTT', 'BOTS', 'Send']);
  await page.fill('.say textarea', '   ');
  await wait(200);
  is('and spaces are not something', (await feet(page)).map((b) => b.what),
    ['Mute Space', 'PTT', 'BOTS']);
  await page.fill('.say textarea', '');
  await wait(200);

  // What you do with the room is over the people in it.
  is('the room\u2019s own buttons are up there',
    await page.evaluate(() =>
      [...document.querySelectorAll('.tools button')].map((b) => b.getAttribute('aria-label'))),
    ['Share screen', 'Leave audio', 'Settings']);

  // The same button whatever it says: Unmute is no wider than Mute.
  const was = box.left;
  await page.click('footer button.big');
  await wait(400);
  const muted = await feet(page);
  is('it says the other thing now', muted[0].what, 'Unmute Space');
  is('and is the same width', muted[0].width, wide);
  is('so nothing beside it moved', (await sayAt(page)).left, was);
  await page.click('footer button.big');
  await wait(400);

  // Nothing is strung along it but those two switches: no sliders, no
  // readouts, nothing to read while you are talking.
  is('no sliders or readouts down there',
    await page.evaluate(() => ({
      sliders: document.querySelectorAll('footer input[type=range]').length,
      boxes: document.querySelectorAll('footer input').length,
      meters: document.querySelectorAll('footer .hear').length,
      switches: document.querySelectorAll('footer .switch').length,
    })),
    { sliders: 0, boxes: 0, meters: 0, switches: 2 });

  // BOTS is on to begin with -- the AIs may answer you until you say
  // otherwise -- and it is yours: pressing it says so on the roster,
  // where everybody can see why an AI answers one person and not the
  // one beside them.
  is('the AIs may answer you to begin with',
    await page.evaluate(() => document.querySelector('footer .bots-switch').getAttribute('aria-pressed')),
    'true');
  await page.click('footer .bots-switch');
  await wait(600);
  is('and turning them off is said on the button',
    await page.evaluate(() => document.querySelector('footer .bots-switch').getAttribute('aria-pressed')),
    'false');
  await page.click('footer .bots-switch');
  await wait(600);
  is('and back on', await page.evaluate(() =>
    document.querySelector('footer .bots-switch').getAttribute('aria-pressed')), 'true');

  // The box grows with what is in it, and stops at half the window.
  const tall = () =>
    page.evaluate(() => Math.round(document.querySelector('.say textarea').getBoundingClientRect().height));
  const one = await tall();
  await page.click('.say textarea');
  await page.keyboard.type('one');
  for (const line of ['two', 'three']) {
    await page.keyboard.down('Shift');
    await page.keyboard.press('Enter');
    await page.keyboard.up('Shift');
    await page.keyboard.type(line);
  }
  await wait(300);
  const three = await tall();
  is('shift and enter make a line rather than sending', await page.inputValue('.say textarea'), 'one\ntwo\nthree');
  is('and the box grew to hold them', three > one, true);
  for (let i = 0; i < 60; i++) {
    await page.keyboard.down('Shift');
    await page.keyboard.press('Enter');
    await page.keyboard.up('Shift');
  }
  await wait(300);
  is('and stops at half the window',
    await page.evaluate(() => {
      const h = document.querySelector('.say textarea').getBoundingClientRect().height;
      return Math.abs(h - window.innerHeight / 2) < 2;
    }),
    true);
  // Enter on its own sends, and the box goes back to one line.
  await page.keyboard.press('Enter');
  await wait(600);
  is('enter sends it', await page.inputValue('.say textarea'), '');
  is('and the box is one line again', await tall(), one);

  // The cog opens a page, and the room is not behind it.
  await page.click('.tools button.cog');
  await page.waitForSelector('.settings');
  is('the settings are a page of their own',
    await page.evaluate(() => ({
      settings: !!document.querySelector('.settings'),
      room: !!document.querySelector('.app'),
      log: !!document.querySelector('.log'),
    })),
    { settings: true, room: false, log: false });
  // What it is for: the things you set once and don't watch. The two
  // switches themselves are not those -- they are what you reach for
  // mid-conversation -- so they stay in the footer, and what each of
  // them *means* when it is off is here, where you settle it once.
  is('what is set here',
    await page.evaluate(() => ({
      name: !!document.querySelector('.settings .name input'),
      volume: !!document.querySelector('.settings input[type=range]'),
      duplex: document.querySelectorAll('.settings input[name=duplex]').length,
      botsOff: document.querySelectorAll('.settings input[name=bots-off]').length,
      switches: document.querySelectorAll('.settings .switch').length,
      meter: !!document.querySelector('.settings .hear'),
    })),
    { name: true, volume: true, duplex: 2, botsOff: 2, switches: 0, meter: false });
  // And out of the box both are the ones that cannot surprise
  // anybody: you go on hearing the room while you hold the key, and
  // turning the AIs off still leaves you able to ask one a question.
  is('the defaults are the forgiving ones',
    await page.evaluate(() => ({
      duplex: [...document.querySelectorAll('.settings input[name=duplex]')].map((r) => r.checked),
      bots: [...document.querySelectorAll('.settings input[name=bots-off]')].map((r) => r.checked),
    })),
    { duplex: [true, false], bots: [true, false] });

  // The room kept running: still joined, and the socket never went.
  is('still in the audio', await page.evaluate(() => !document.querySelector('.enter')), true);
  await page.click('.settings button.primary');
  await wait(500);
  is('back in the room', await page.evaluate(() => !!document.querySelector('.log')), true);

  // Push to talk, beside the button it changes: ticking it is what
  // turns that button into one you hold.
  await page.click('footer .ptt-switch');
  await wait(400);
  is('the button says to hold it', (await feet(page))[0].what, 'Hold to talk Space');
  is('and is the same width', (await feet(page))[0].width, wide);
  await page.click('footer .ptt-switch');
  await wait(400);
  is('and back again', (await feet(page))[0].what, 'Mute Space');

  // Escape closes it too, and the cog says whether it is open.
  await page.click('.tools button.cog');
  await page.waitForSelector('.settings');
  await page.keyboard.press('Escape');
  await wait(500);
  is('Escape closes it', await page.evaluate(() => !!document.querySelector('.settings')), false);
  is('and the cog is not pressed',
    await page.evaluate(() => document.querySelector('.tools button.cog').getAttribute('aria-pressed')),
    'false');

  // And the way out is still the way out: leaving from the room puts
  // you at the door, and coming back lands in the conversation rather
  // than in the settings somebody left open.
  await page.click('.tools button.leave');
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
