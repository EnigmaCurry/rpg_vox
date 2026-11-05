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

// The phone's talk key, as the Android app actually presses it. The two
// snippets are read out of the app rather than copied here, because
// copies of a selector do not break when the selector does: the app
// reaches into `footer button.big`, its `ptt` class, the label beside
// the key hint and the room's own `vox_room.talk_key`, and every one of
// those is this room's private business. If the room moves one of them,
// the phone goes deaf and this is where it is heard. See ROOM_ANDROID.md.
const KT = require('path').join(__dirname,
  '../../../android-room/app/src/main/java/com/enigmacurry/vox_room/MainActivity.kt');
const kt = require('fs').readFileSync(KT, 'utf8');
const between = (after) => {
  const at = kt.indexOf(after);
  if (at < 0) throw new Error(`the app no longer has ${after.trim()}`);
  return kt.slice(at + after.length, kt.indexOf('\"\"\"', at + after.length));
};
const STATE_JS = between('private const val STATE_JS = \"\"\"');
// `talkJs` is a Kotlin template, so the `${if (down) ... else ...}` in it
// are resolved here the way Kotlin would resolve them. Generically, and
// then checked: a snippet that still has a `${` in it would reach the
// page as a syntax error, which says nothing about what actually broke.
const talkJs = (down) => {
  const js = between('private fun talkJs(down: Boolean) = \"\"\"').replace(
    /\$\{if \(down\) ("(?:[^"\\]|\\.)*") else ("(?:[^"\\]|\\.)*")\}/g,
    (_, yes, no) => JSON.parse(down ? yes : no));
  if (js.includes('${')) throw new Error('the app presses the key with a Kotlin template this script cannot resolve');
  return js;
};

/// Hold the space bar and see whether the mic opened.
/// Longer than the quarter second push to talk arms for before the mic
/// goes live -- the chirp fills that wait; see `lib/chirp.js`. Waiting
/// exactly as long as the arming would be a race with it.
const space = async (page) => {
  await page.keyboard.down('Space');
  await page.waitForTimeout(500);
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
  await page.click('footer .ptt-switch');
  await page.waitForTimeout(400);
  is('the talk button is there to hold', await page.evaluate(() => !!document.querySelector('footer .big.ptt')), true);
  is('and the mic is not open until it is', await page.evaluate(() => !!document.querySelector('footer .big.ptt.talking')), false);

  // Nothing focused: the case that always worked.
  await page.evaluate(() => document.activeElement?.blur());
  is('Space talks', await space(page), true);

  // The quarter second a hold arms for before it is live. The chirp
  // fills it and is what it is for -- see `lib/chirp.js` -- and the
  // microphone is still shut for the whole of it, which is the half
  // that matters: a chirp that played while you were already live would
  // go out to the room on a speakerphone with every first word.
  await page.keyboard.down('Space');
  await page.waitForTimeout(120);
  is('a hold is not live while the chirp is still going',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt.talking')), false);
  await page.waitForTimeout(400);
  is('and is live once it has finished',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt.talking')), true);
  await page.keyboard.up('Space');
  await page.waitForTimeout(900);

  // Let go inside it and nothing was ever said: no microphone, and
  // nothing for the room to have written down.
  await page.keyboard.down('Space');
  await page.waitForTimeout(80);
  await page.keyboard.up('Space');
  await page.waitForTimeout(600);
  is('letting go inside the arming never opens the mic at all',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt.talking')), false);

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
    await page.evaluate(() => document.querySelector('footer .big.ptt')?.innerText.includes('mixer')), true);
  is('and Space does not talk', await space(page), false);
  await page.click('.rooms .strip button[aria-pressed]');
  await page.waitForTimeout(400);
  is('unmuted, the key is yours again', await space(page), true);

  // What holding the key does to the room's own sound, which is a
  // setting and not a given: full duplex out of the box (you go on
  // hearing everybody, which is what a headset wants), half duplex if
  // you ask for it on the settings page.
  await page.evaluate(() => document.activeElement?.blur());
  // What the button says is one thing and what the speakers do is
  // another, so the gain itself is watched: every ramp any audio
  // parameter is asked for, recorded off the prototype, which is the
  // one place this can be seen from outside the app.
  // Page-wide, so anything else on this page that moves a gain with a
  // linear ramp lands in here too and reads as the room's sound. The
  // chirp deliberately shapes its envelope with `setValueCurveAtTime`
  // for exactly that reason; see `lib/chirp.js`.
  await page.evaluate(() => {
    window.__ramps = [];
    const ramp = AudioParam.prototype.linearRampToValueAtTime;
    AudioParam.prototype.linearRampToValueAtTime = function (value, when) {
      window.__ramps.push(value);
      return ramp.call(this, value, when);
    };
  });

  // The default: nothing is cut. Holding the key to interrupt and not
  // hearing the AI stop is the cost of the other way round, and it is
  // not a cost anybody on headphones is paying without asking.
  await page.keyboard.down('Space');
  await page.waitForTimeout(500);
  is('the room is still playing while you hold, out of the box',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt.hushed')), false);
  is('and nothing was taken to nothing',
    await page.evaluate(() => window.__ramps.includes(0)), false);
  await page.keyboard.up('Space');
  await page.waitForTimeout(800);

  // And the other way round, asked for where it is set: the room goes
  // quiet while the key is down, which is what somebody on speakers
  // with a microphone pointed at them holds it for. The mic cannot be
  // made deaf to one sound in the room it is in, so the room stops
  // making it -- and starts again only once the mic has actually
  // closed, since the tail after letting go is still a moment with a
  // microphone in front of the speakers.
  await page.click('.tools button.cog');
  await page.waitForSelector('.settings');
  await page.locator('.settings input[name=duplex]').nth(1).check();
  await page.click('.settings button.primary');
  await page.waitForSelector('footer .big.ptt');
  await page.evaluate(() => document.activeElement?.blur());
  await page.keyboard.down('Space');
  await page.waitForTimeout(500);
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

  // The setting is a page away from the key, and it still lands on the
  // hold you are in: the room is still running behind the settings
  // page, the space bar still talks from there, so changing it there
  // while holding it has to be heard at once rather than next time.
  await page.evaluate(() => { window.__ramps = []; });
  await page.keyboard.down('Space');
  await page.waitForTimeout(500);
  await page.click('.tools button.cog');
  await page.waitForSelector('.settings');
  await page.locator('.settings input[name=duplex]').nth(0).check();
  await page.waitForTimeout(150);
  is('the room comes back mid-hold when you ask to keep hearing it',
    await page.evaluate(() => window.__ramps.at(-1)), 1);
  await page.locator('.settings input[name=duplex]').nth(1).check();
  await page.waitForTimeout(150);
  is('and goes again when you ask for the other way',
    await page.evaluate(() => window.__ramps.at(-1)), 0);
  await page.keyboard.up('Space');
  await page.waitForTimeout(800);
  await page.click('.settings button.primary');
  await page.waitForSelector('footer .big.ptt');

  // And turning the mode off mid-hold is not a room left silent: the
  // switch is beside the button, which is where a hand goes.
  await page.evaluate(() => document.activeElement?.blur());
  await page.keyboard.down('Space');
  await page.waitForTimeout(500);
  await page.click('footer .ptt-switch');
  await page.keyboard.up('Space');
  await page.waitForTimeout(400);
  is('letting go of push to talk while holding it gives the room back',
    await page.evaluate(() => !!document.querySelector('footer .big.hushed')), false);
  await page.click('footer .ptt-switch');
  await page.waitForTimeout(400);

  // Where Space is not yours: typing it types it.
  await page.click('.say textarea');
  await page.fill('.say textarea', 'a');
  is('the box has it instead', await space(page), false);
  is('and it went in as a space', await page.inputValue('.say textarea'), 'a ');

  // And Escape is the way back out of it. The box keeps the focus
  // after you have finished with it, so without this the space bar
  // stays the box's until you think to click the background.
  await page.keyboard.press('Escape');
  await page.waitForTimeout(200);
  is('Escape lets the box go', await focused(page), 'body');
  is('and what you had typed is still there', await page.inputValue('.say textarea'), 'a ');
  is('and the key is the microphone\u2019s again', await space(page), true);
  await page.fill('.say textarea', '');

  // The other thing that takes it: a menu on a fader, where Space
  // opens the list rather than the microphone. The first Escape here
  // shuts the list the click opened, which leaves the state anybody
  // who has just set `Stops` is actually in -- list closed, menu still
  // holding the focus.
  await page.click('.rooms .when select');
  await page.keyboard.press('Escape');
  await page.waitForTimeout(200);
  is('a menu keeps the focus after it is used', (await focused(page)).startsWith('select'), true);
  is('and Space is not yours while it has it', await space(page), false);
  // That Space opened the list again, and the browser keeps the Escape
  // that shuts a list it opened -- the page is never told about that
  // one. The next press is the one that reaches us.
  await page.keyboard.press('Escape');
  await page.waitForTimeout(150);
  await page.keyboard.press('Escape');
  await page.waitForTimeout(200);
  is('Escape lets that go too', await focused(page), 'body');
  is('and the key is yours again', await space(page), true);

  // Escape in a box is about the box and nothing else: the settings
  // page has its own Escape, and a press meant for the name field must
  // not take the page out from under it.
  await page.click('.tools button.cog');
  await page.waitForSelector('.settings');
  await page.click('.settings .name input');
  await page.keyboard.press('Escape');
  await page.waitForTimeout(300);
  is('the settings page is still open', await page.evaluate(() => !!document.querySelector('.settings')), true);
  is('with the box let go of', await focused(page), 'body');
  await page.keyboard.press('Escape');
  await page.waitForTimeout(400);
  is('and the next press leaves it', await page.evaluate(() => !!document.querySelector('.settings')), false);
  await page.waitForSelector('footer .big.ptt');

  // A dialog answers with Space, so it keeps it.
  await page.click('.bar button:has-text("New conversation")');
  await page.waitForTimeout(500);
  is('a dialog is asking', await page.evaluate(() => !!document.querySelector('dialog.ask[open]')), true);
  is('and Space is the dialog’s', await space(page), false);
  await page.keyboard.press('Escape');
  await page.waitForTimeout(400);

  // Which key talks is a choice, and the one thing that must survive
  // it is letting go. Bound to Ctrl+Space and released modifier-first,
  // the keyup for Space arrives with `ctrlKey` already false -- a
  // release that insisted on the modifiers would never match it, and
  // the microphone would stay open with nothing on the screen saying
  // so. So the room listens for the key alone coming up.
  await page.evaluate(() => document.activeElement?.blur());
  await page.click('.tools button.cog');
  await page.waitForSelector('.settings');
  await page.click('.settings .press');
  is('the room is waiting for a key',
    await page.evaluate(() => document.querySelector('.settings .press').textContent.trim()),
    'Press a key…');

  // The three kinds of no. A key the room is simply never given needs
  // no list -- it never arrives -- so these are the ones that do.
  await page.keyboard.press('Control+KeyW');
  await page.waitForTimeout(200);
  is('a key the browser keeps is refused',
    await page.evaluate(() => document.querySelector('.settings .warn')?.textContent.includes('closes the tab')), true);
  await page.keyboard.press('Tab');
  await page.waitForTimeout(200);
  is('and so is one the room has already spent',
    await page.evaluate(() => document.querySelector('.settings .warn')?.textContent.includes('Tab')), true);
  is('and it is still waiting, a no being about that key and not the end of it',
    await page.evaluate(() => document.querySelector('.settings .press').textContent.trim()),
    'Press a key…');

  await page.keyboard.down('Control');
  await page.keyboard.press('Space');
  await page.keyboard.up('Control');
  await page.waitForTimeout(300);
  is('a combination binds', await page.evaluate(() => document.querySelector('.settings .press kbd')?.textContent), 'Ctrl+Space');
  await page.click('.settings button.primary');
  await page.waitForSelector('footer .big.ptt');
  is('and the talk button says which key it is now',
    await page.evaluate(() => document.querySelector('footer .big.ptt kbd')?.textContent), 'Ctrl+Space');

  await page.evaluate(() => document.activeElement?.blur());
  is('the bare key no longer talks', await space(page), false);

  await page.keyboard.down('Control');
  await page.keyboard.down('Space');
  await page.waitForTimeout(500);
  is('the bound one does',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt.talking')), true);
  // The modifier first, which is how a hand actually lets go.
  await page.keyboard.up('Control');
  await page.keyboard.up('Space');
  await page.waitForTimeout(900);
  is('and letting the modifier go first still closes the mic',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt.talking')), false);

  // Back to where the rest of this suite left it.
  await page.click('.tools button.cog');
  await page.waitForSelector('.settings');
  await page.click('.settings .bind button:has-text("Back to Space")');
  await page.waitForTimeout(200);
  is('and it can be put back', await page.evaluate(() => document.querySelector('.settings .press kbd')?.textContent), 'Space');
  await page.click('.settings button.primary');
  await page.waitForSelector('footer .big.ptt');
  await page.evaluate(() => document.activeElement?.blur());
  is('with the space bar talking again', await space(page), true);

  // The button does not change width, whatever it says and whatever
  // key is bound. It is the one control here you put a hand on without
  // looking, so a button that is one size while it says `Hold to talk`
  // and another the moment it says `Talking...` moves out from under
  // you mid-sentence -- and takes the switches beside it along.
  const wide = () => page.evaluate(() => Math.round(
    document.querySelector('footer .big').getBoundingClientRect().width));
  const sizes = {};
  await page.evaluate(() => document.activeElement?.blur());
  sizes[await page.evaluate(() => document.querySelector('footer .big').innerText.trim())] = await wide();
  await page.keyboard.down('Space');
  await page.waitForTimeout(500);
  sizes.talking = await wide();
  await page.keyboard.up('Space');
  await page.waitForTimeout(800);
  await page.click('footer .ptt-switch');
  await page.waitForTimeout(400);
  sizes.mute = await wide();
  await page.click('footer .big');
  await page.waitForTimeout(300);
  sizes.unmute = await wide();
  await page.click('footer .big');
  await page.waitForTimeout(300);
  await page.click('.rooms .strip button[aria-pressed]');
  await page.waitForTimeout(400);
  sizes.mixer = await wide();
  await page.click('.rooms .strip button[aria-pressed]');
  await page.waitForTimeout(400);
  await page.click('footer .ptt-switch');
  await page.waitForTimeout(400);
  is('one width for everything it can say', new Set(Object.values(sizes)).size, 1);

  // Letting go of the key is heard wherever the focus went in between.
  await page.evaluate(() => document.activeElement?.blur());
  await page.keyboard.down('Space');
  await page.waitForTimeout(500);
  await page.click('.say textarea');
  await page.keyboard.up('Space');
  await page.waitForTimeout(900);
  is('letting go closes the mic even after the focus moved',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt.talking')), false);

  // -- back ----------------------------------------------------------
  //
  // Back on a phone is the gesture Escape is on a keyboard. The room
  // puts pages and panels over itself that the WebView never navigated
  // to, so the app cannot ask the browser whether there is anywhere
  // back to go -- `canGoBack` is false inside every one of them, and
  // back used to mean leave, which put the whole app away from the
  // settings page. It asks the room what it has open instead. Read out
  // of the app like the snippets above, for the same reason.
  const CLOSE_JS = between('private const val CLOSE_JS = \"\"\"');
  is('with nothing over the room, back is not the room\'s to take',
    await page.evaluate(CLOSE_JS), 'nothing');

  await page.click('.tools button.cog');
  await page.waitForSelector('.settings');
  is('the settings page is a layer to close', await page.evaluate(CLOSE_JS), 'closed');
  await page.waitForTimeout(400);
  is('and back closed it rather than the app',
    await page.evaluate(() => !!document.querySelector('main.settings')), false);
  await page.waitForSelector('footer .big');

  // A panel over the room is a layer too, and the same press shuts it.
  await page.click('.picker > button');
  await page.waitForTimeout(600);
  is('a panel over the room is one as well', await page.evaluate(CLOSE_JS), 'closed');
  await page.waitForTimeout(400);
  is('and that shut too',
    await page.evaluate(() => !!document.querySelector('.menu')), false);
  is('with nothing left over the room', await page.evaluate(CLOSE_JS), 'nothing');

  // -- the phone -----------------------------------------------------
  //
  // A phone has no keyboard, so the Android app lends the microphone a
  // volume key. Android never gives a volume key to the page, so the
  // app has to press the room's own talk key from outside it -- and it
  // has to know, synchronously, whether that key is the microphone's
  // at all, which it reads off the one button it is all written on.
  await page.evaluate(() => document.activeElement?.blur());
  const asked = async () => JSON.parse(await page.evaluate(STATE_JS));

  let state = await asked();
  is('the app can see that push to talk is on', state.ptt, true);
  is('and offers the notification no mute while it is', state.says, '');
  is('and knows the key is not being typed with', state.typing, false);

  // Still 250, deliberately, where every press above waits 500: the
  // app has already done the arming -- it spent a quarter of a second
  // working out that a volume key was a hold and not a tap, and said so
  // -- so this press must go live at once. Arming it again would make a
  // phone wait twice for one hold, and this is the assertion that would
  // catch it.
  await page.evaluate(talkJs(true));
  await page.waitForTimeout(250);
  is('a volume key talks without waiting to be armed again',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt.talking')), true);
  await page.evaluate(talkJs(false));
  await page.waitForTimeout(900);
  is('and letting it go closes the mic',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt.talking')), false);

  // Where the volume key is the phone's again: with the cursor in the
  // message box the room would refuse the press anyway, so the app
  // stands down first and leaves the key changing the volume.
  await page.click('.say textarea');
  is('the app stands down over a text box', (await asked()).typing, true);
  await page.keyboard.press('Escape');
  await page.waitForTimeout(200);
  is('and takes the key back when the box lets go', (await asked()).typing, false);

  // Whichever key the room is bound to, and not Space by assumption:
  // the app reads the binding out of the room's own storage, because a
  // volume key pressing a key the room is not listening for is a
  // microphone that never opens and nothing on the screen saying why.
  await page.click('.tools button.cog');
  await page.waitForSelector('.settings');
  await page.click('.settings .press');
  await page.keyboard.down('Control');
  await page.keyboard.press('Space');
  await page.keyboard.up('Control');
  await page.waitForTimeout(300);
  await page.click('.settings button.primary');
  await page.waitForSelector('footer .big.ptt');
  await page.evaluate(() => document.activeElement?.blur());
  await page.evaluate(talkJs(true));
  await page.waitForTimeout(250);
  is('a volume key talks on a bound combination too',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt.talking')), true);
  await page.evaluate(talkJs(false));
  await page.waitForTimeout(900);
  is('and still lets go of it',
    await page.evaluate(() => !!document.querySelector('footer .big.ptt.talking')), false);
  await page.click('.tools button.cog');
  await page.waitForSelector('.settings');
  await page.click('.settings .bind button:has-text("Back to Space")');
  await page.click('.settings button.primary');
  await page.waitForSelector('footer .big.ptt');

  // Push to talk off: the key goes back to the phone, and the one
  // thing the app reads off that button is what its notification
  // should offer instead.
  await page.click('footer .ptt-switch');
  await page.waitForTimeout(400);
  state = await asked();
  is('with push to talk off the app lends no key', state.ptt, false);
  is('and reads the mute off the button instead',
    ['Mute', 'Unmute'].includes(state.says), true);

  // And a press that arrives anyway is refused. Out on the phone the
  // mode is known a second late at worst, and the room does something
  // quite different with the talk key when it is off -- it toggles your
  // mute. A volume key that muted you because the switch had just been
  // thrown is the one way this could do real harm, so the press asks
  // the page rather than trusting what it last heard.
  const muted = () => page.evaluate(() => !!document.querySelector('footer .big.muted'));
  const before = await muted();
  await page.evaluate(talkJs(true));
  await page.waitForTimeout(400);
  is('and a press with the mode off does not mute you', await muted(), before);
  await page.evaluate(talkJs(false));
  await page.waitForTimeout(200);

  await page.click('footer .ptt-switch');
  await page.waitForTimeout(400);

  // Where the phone's key is actually settled: the room's own settings
  // page, kept against you on the server rather than in this browser,
  // so a second phone arrives already knowing. The room mirrors what
  // the server said into storage and the app reads it from there in
  // the poll above -- which is the whole mechanism, and is why the app
  // can take a setting from the room without the room being able to
  // call into the app. See `rememberPhone` and ROOM_ANDROID.md.
  const mirror = () => page.evaluate(() => [
    localStorage.getItem('vox_room.volume_talk'),
    localStorage.getItem('vox_room.ptt_chirp'),
  ]);
  is('nothing is said about the phone to begin with', (await asked()).volume, '');

  await page.click('.tools button.cog');
  await page.waitForSelector('.settings');
  await page.locator('.settings input[name=volumetalk]').uncheck();
  await page.waitForTimeout(500);
  is('settling it in the room writes the mirror the app reads',
    await mirror(), ['off', 'on']);
  is('and the app sees it', (await asked()).volume, 'off');

  await page.locator('.settings input[name=pttchirp]').uncheck();
  await page.waitForTimeout(500);
  is('and so does turning the chirp off', (await asked()).chirp, 'off');
  is('with the volume keys left where they were', (await asked()).volume, 'off');

  // It outlives this browser, because it was never this browser's.
  // The mirror is wiped before the reload on purpose: `localStorage`
  // survives a reload on its own, so leaving it there would prove
  // nothing about where the answer actually lives. Emptied and then
  // refilled is the round trip to the server and back.
  await page.evaluate(() => {
    localStorage.removeItem('vox_room.volume_talk');
    localStorage.removeItem('vox_room.ptt_chirp');
  });
  await page.reload();
  // The door, not the room: a reload is back outside the audio, and
  // the answer is already there before anybody joins anything.
  await page.waitForSelector('button:has-text("Join Audio")');
  await page.waitForTimeout(500);
  is('and it is put back from the server, not from this browser',
    await mirror(), ['off', 'off']);

  await browser.close();
  report();
})();
