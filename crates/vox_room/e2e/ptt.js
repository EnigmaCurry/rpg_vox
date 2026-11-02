// The space bar belongs to your microphone:
//   node ptt.js RYAN_LINK
//
// Push to talk is held, so what matters is that the key reaches the
// microphone from wherever you left the focus. A button keeps the focus
// after you click it, and buttons are most of this room -- a room in
// the list, a mute, somebody's shared screen -- so a space bar that
// only worked over the background was a space bar that stopped working
// as soon as you touched anything. The exceptions are where Space is
// typing or answering: a text box, and a dialog.
//
//   vox_room --room Lobby --room Studio --echo --no-stt --tts none

const { is, report, person, wait, openRooms, closeRooms } = require('./room.js');
const [ryanLink] = process.argv.slice(2);

/// Hold the space bar and see whether the mic opened.
const space = async (page) => {
  await page.keyboard.down('Space');
  await page.waitForTimeout(250);
  const talking = await page.evaluate(() => !!document.querySelector('footer .big.ptt.talking'));
  await page.keyboard.up('Space');
  // The mic stays open a moment after you let go, so the next check
  // isn't looking at the tail of this one.
  await page.waitForTimeout(700);
  return talking;
};

/// What has the focus, as a selector you can read.
const focused = (page) =>
  page.evaluate(() => {
    const e = document.activeElement;
    if (!e || e === document.body) return 'body';
    return `${e.tagName.toLowerCase()}${e.textContent ? `: ${e.textContent.replace(/\s+/g, ' ').trim().slice(0, 24)}` : ''}`;
  });

(async () => {
  const { browser, page } = await person(ryanLink);

  // Push to talk is beside the button it changes.
  await page.click('footer .switch input');
  await page.waitForTimeout(400);
  is('the talk button is there to hold', await page.evaluate(() => !!document.querySelector('footer .big.ptt')), true);
  is('and the mic is not open until it is', await page.evaluate(() => !!document.querySelector('footer .big.ptt.talking')), false);

  // Nothing focused: the case that always worked.
  await page.evaluate(() => document.activeElement?.blur());
  is('Space talks', await space(page), true);

  // The heading that names the room you are in, which is a button and
  // is where the hand goes to look for another room. (Pressing a room
  // *in* the list is not this case: the list shuts and takes the
  // button with it, so nothing is left holding the focus.)
  await openRooms(page);
  is('a button keeps the focus', (await focused(page)).startsWith('button'), true);
  is('and Space still talks', await space(page), true);
  await closeRooms(page);

  // And it doesn't press that button again while you hold it: the
  // history panel opened by a click stays open through a word.
  await page.click('.picker > button');
  await page.waitForTimeout(600);
  is('the history is open', await page.evaluate(() => !!document.querySelector('.menu')), true);
  is('Space talks with it open', await space(page), true);
  is('and the space bar did not press the button again',
    await page.evaluate(() => !!document.querySelector('.menu')), true);
  await page.keyboard.press('Escape');
  await page.waitForTimeout(400);

  // A fader's mute is a button too, and the one most likely to be under
  // your hand.
  await page.click('.rooms .strip button[aria-pressed]');
  await page.waitForTimeout(400);
  is('Space talks from a mute', await space(page), true);
  await page.click('.rooms .strip button[aria-pressed]');
  await page.waitForTimeout(400);

  // Where Space is not yours: typing it types it.
  await page.click('.say textarea');
  await page.fill('.say textarea', 'a');
  is('the box has it instead', await space(page), false);
  is('and it went in as a space', await page.inputValue('.say textarea'), 'a ');
  await page.fill('.say textarea', '');

  // A dialog answers with Space, so it keeps it.
  await page.click('.bar button:has-text("New conversation")');
  await page.waitForTimeout(500);
  is('a dialog is asking', await page.evaluate(() => !!document.querySelector('dialog.ask[open]')), true);
  is('and Space is the dialog’s', await space(page), false);
  await page.keyboard.press('Escape');
  await page.waitForTimeout(400);

  // Letting go of the key is heard wherever the focus went in between.
  await page.evaluate(() => document.activeElement?.blur());
  await page.keyboard.down('Space');
  await page.waitForTimeout(250);
  await page.click('.say textarea');
  await page.keyboard.up('Space');
  await page.waitForTimeout(900);
  is('letting go closes the mic even after the focus moved',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt.talking')), false);

  await browser.close();
  report();
})();
