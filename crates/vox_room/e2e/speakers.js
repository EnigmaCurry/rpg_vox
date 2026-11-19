// Speakers chosen at the door, and a room that plays through others:
//   node speakers.js RYAN_LINK
//
// Every room plays through the speakers chosen at the door, unless this
// browser has been told that one room plays through others. That is
// remembered for that room alone, so walking out of it goes back to the
// door's and walking back in goes back to its own -- and so does coming
// back another day. A room's speakers that are unplugged are not
// forgotten, only not used until they are plugged in again.
//
// A headless Chromium has no speakers to choose between, so the page is
// handed two that do not exist, and `setSinkId` writes down where it
// was asked to play rather than trying to: what is checked is the
// choosing, not the sound.
//
// Twice: once the way Chrome does it, with the AudioContext pointed at
// the speakers, and once the way Firefox has to, with the context's own
// `setSinkId` taken away and the room's sound played by an `<audio>`
// element that is pointed instead -- and only while it is going
// somewhere other than the system default.
//
//   vox_room --room Hall --room Lobby --room Studio --echo --no-stt --tts none

const { is: check, report, launch, comeIn, goTo, whereYouAre, wait } = require('./room.js');
const [ryanLink] = process.argv.slice(2);

const ROOM_SELECT = '.rooms label.speaker select';
const DOOR_SELECT = '.enter label:has(span:text-is("Speakers")) select';

/// Two speakers the page can be pointed at, and a record of where it was.
function fakeSpeakers() {
  window.__sinks = [];
  window.__deskPlugged = true;
  AudioContext.prototype.setSinkId = function (id) {
    window.__sinks.push(id);
    return Promise.resolve();
  };
  const real = MediaDevices.prototype.enumerateDevices;
  MediaDevices.prototype.enumerateDevices = async function () {
    const devices = await real.call(this);
    const out = (deviceId, label) => ({ deviceId, label, kind: 'audiooutput', groupId: deviceId });
    return [
      ...devices.filter((d) => d.kind !== 'audiooutput'),
      out('headset', 'Headset'),
      ...(window.__deskPlugged ? [out('desk', 'Desk speakers')] : []),
    ];
  };
}

/// Firefox, as far as the room can tell: no `AudioContext.setSinkId`,
/// and an `HTMLMediaElement.setSinkId` that writes down which element
/// was pointed where.
function likeFirefox() {
  window.__firefox = true;
  delete AudioContext.prototype.setSinkId;
  window.__els = new Set();
  HTMLMediaElement.prototype.setSinkId = function (id) {
    window.__els.add(this);
    this.__sink = id;
    return Promise.resolve();
  };
}

/// Where the room is playing just now. Chrome's way, the last place the
/// context was sent. Firefox's, the element still playing the room's
/// stream, if there is one -- none at all being the system default,
/// which in this suite is never where the room is meant to be.
const playing = (page) =>
  page.evaluate(() => {
    if (!window.__firefox) return window.__sinks.at(-1) ?? null;
    const on = [...window.__els].filter((e) => e.srcObject && !e.paused);
    return on.length === 1 ? on[0].__sink : on.length ? 'several' : 'default';
  });
const stored = (page) =>
  page.evaluate(() => JSON.parse(localStorage.getItem('vox_room.speaker.rooms') ?? '{}'));

async function speakersHere(page) {
  await page.waitForSelector(ROOM_SELECT);
  await wait(400);
}

async function plugDesk(page, on) {
  await page.evaluate((on) => {
    window.__deskPlugged = on;
    navigator.mediaDevices.dispatchEvent(new Event('devicechange'));
  }, on);
  await wait(400);
}

(async () => {
  const browser = await launch();
  const ctx = await browser.newContext({ permissions: ['microphone'] });
  await ctx.addInitScript(fakeSpeakers);
  // The invite is spent by the first pass; the second is the same
  // browser, so it comes in on the cookie the first was given.
  await scenario(await ctx.newPage(), 'chrome', ryanLink);
  const firefox = await ctx.newPage();
  await firefox.addInitScript(likeFirefox);
  await scenario(firefox, 'firefox', ryanLink);
  await browser.close();
  report();
})().catch((e) => {
  console.error(e);
  process.exit(1);
});

async function scenario(page, how, link) {
  page.on('pageerror', (e) => console.log('[pageerror]', e.message));
  const is = (what, got, want) => check(`${how}: ${what}`, got, want);
  await page.goto(link);
  await page.waitForSelector(DOOR_SELECT);
  await wait(600);
  await page.selectOption(DOOR_SELECT, 'headset');
  await comeIn(page);
  await goTo(page, 'Lobby');
  is('the door chose the headset, so the room plays there', await playing(page), 'headset');

  await speakersHere(page);
  is('this room has no speakers of its own yet', await page.inputValue(ROOM_SELECT), '');
  is('and says which ones it is borrowing from the door',
    await page.locator(`${ROOM_SELECT} option[value=""]`).textContent(),
    'Same as the door: Headset');
  await page.selectOption(ROOM_SELECT, 'desk');
  await wait(300);
  is('choosing the desk for this room plays it there', await playing(page), 'desk');
  is('and is written down against this room', await stored(page), { Lobby: 'desk' });

  await goTo(page, 'Studio');
  is('another room plays through the door\'s', await playing(page), 'headset');
  await speakersHere(page);
  is('and has nothing of its own', await page.inputValue(ROOM_SELECT), '');

  await goTo(page, 'Lobby');
  is('walking back in plays through the desk again', await playing(page), 'desk');

  // Another day: the door again, and the same choice already made.
  await page.reload();
  await comeIn(page);
  if ((await whereYouAre(page)) !== 'Lobby') await goTo(page, 'Lobby');
  is('a room remembers its speakers across a reload', await playing(page), 'desk');

  await speakersHere(page);
  is('and the room says so', await page.inputValue(ROOM_SELECT), 'desk');
  await plugDesk(page, false);
  is('desk unplugged: back to the door\'s', await playing(page), 'headset');
  is('but the choice is kept', await page.inputValue(ROOM_SELECT), 'desk');
  is('and the page says why the sound moved',
    (await page.locator('.rooms .speaker + .wearing').textContent()).includes('aren\'t plugged in'),
    true);
  await plugDesk(page, true);
  is('plugged back in: the desk again', await playing(page), 'desk');

  await page.selectOption(ROOM_SELECT, '');
  await wait(300);
  is('back to the door\'s speakers', await playing(page), 'headset');
  is('and nothing is left written down', await stored(page), {});

  // The system default at the door: Chrome points the context back at
  // it, and Firefox has no element playing the room at all.
  await page.reload();
  await page.waitForSelector(DOOR_SELECT);
  await wait(600);
  await page.selectOption(DOOR_SELECT, '');
  await comeIn(page);
  if ((await whereYouAre(page)) !== 'Studio') await goTo(page, 'Studio');
  is('the system default plays the room the plain way', await playing(page),
    how === 'firefox' ? 'default' : '');

  await page.close();
}
