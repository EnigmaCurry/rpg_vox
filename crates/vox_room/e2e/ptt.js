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
  // your hand. Clicked twice, so the strip is live again while the
  // focus is still sitting on it: what this is about is where the key
  // goes, not what the fader does.
  await page.click('.rooms .strip button[aria-pressed]');
  await page.waitForTimeout(400);
  await page.click('.rooms .strip button[aria-pressed]');
  await page.waitForTimeout(400);
  is('Space talks from a mute', await space(page), true);

  // What the fader does is the other thing, and the first strip is
  // your own: muted there, the room is not listening to you at all --
  // nothing you say is transcribed or heard. So the talk button has to
  // be dead and say why, rather than going green over audio that is
  // being thrown away. A button that lights up is the room claiming to
  // be listening, and it has to be true.
  await page.click('.rooms .strip button[aria-pressed]');
  await page.waitForTimeout(400);
  is('the talk button is dead while the room has you muted',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt[disabled]')), true);
  is('and says which mute it is',
    await page.evaluate(() => document.querySelector('footer .big.ptt')?.textContent.includes('mixer')), true);
  is('and Space does not talk', await space(page), false);
  await page.click('.rooms .strip button[aria-pressed]');
  await page.waitForTimeout(400);
  is('unmuted, the key is yours again', await space(page), true);

  // The room goes quiet while the key is down, which is the whole
  // point of the mode: somebody on speakers with a microphone pointed
  // at them. The mic cannot be made deaf to one sound in the room it
  // is in, so the room stops making it -- and starts again only once
  // the mic has actually closed, since the tail after letting go is
  // still a moment with a microphone in front of the speakers.
  await page.evaluate(() => document.activeElement?.blur());
  // What the button says is one thing and what the speakers do is
  // another, so the gain itself is watched: every ramp any audio
  // parameter is asked for, recorded off the prototype, which is the
  // one place this can be seen from outside the app.
  await page.evaluate(() => {
    window.__ramps = [];
    const ramp = AudioParam.prototype.linearRampToValueAtTime;
    AudioParam.prototype.linearRampToValueAtTime = function (value, when) {
      window.__ramps.push(value);
      return ramp.call(this, value, when);
    };
  });
  await page.keyboard.down('Space');
  await page.waitForTimeout(250);
  is('the room is quiet while the key is down',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt.hushed')), true);
  is('and the sound really was taken to nothing',
    await page.evaluate(() => window.__ramps.at(-1)), 0);
  await page.keyboard.up('Space');
  await page.waitForTimeout(100);
  is('and still quiet through the moment the mic is left open for',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt.hushed')), true);
  await page.waitForTimeout(800);
  is('and comes back once the mic has closed',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt.hushed')), false);
  is('with the sound brought back up where it was',
    await page.evaluate(() => window.__ramps.at(-1)), 1);

  // And turning the mode off mid-hold is not a room left silent: the
  // switch is beside the button, which is where a hand goes.
  await page.keyboard.down('Space');
  await page.waitForTimeout(250);
  await page.click('footer .switch input');
  await page.keyboard.up('Space');
  await page.waitForTimeout(400);
  is('letting go of push to talk while holding it gives the room back',
    await page.evaluate(() => !!document.querySelector('footer .big.hushed')), false);
  await page.click('footer .switch input');
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
