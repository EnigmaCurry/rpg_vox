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
//   vox_room --room Hall --room Lobby --room Studio --echo --no-stt --tts none

const { is, report, launch, comeIn, goTo, whereYouAre, wait } = require('./room.js');
const [ryanLink] = process.argv.slice(2);

const ROOM_SELECT = '.settings label.speaker select';
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

/// Where the room is playing just now: the last place it was sent.
const playing = (page) => page.evaluate(() => window.__sinks.at(-1) ?? null);
const stored = (page) =>
  page.evaluate(() => JSON.parse(localStorage.getItem('vox_room.speaker.rooms') ?? '{}'));

async function openSettings(page) {
  await page.click('.tools button.cog');
  await page.waitForSelector(ROOM_SELECT);
  await wait(400);
}

async function closeSettings(page) {
  await page.keyboard.press('Escape');
  await page.waitForSelector('.settings', { state: 'detached' });
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
  const page = await ctx.newPage();
  page.on('pageerror', (e) => console.log('[pageerror]', e.message));

  await page.goto(ryanLink);
  await page.waitForSelector(DOOR_SELECT);
  await wait(600);
  await page.selectOption(DOOR_SELECT, 'headset');
  await comeIn(page);
  await goTo(page, 'Lobby');
  is('the door chose the headset, so the room plays there', await playing(page), 'headset');

  await openSettings(page);
  is('this room has no speakers of its own yet', await page.inputValue(ROOM_SELECT), '');
  is('and says which ones it is borrowing from the door',
    await page.locator(`${ROOM_SELECT} option[value=""]`).textContent(),
    'Same as the door: Headset');
  await page.selectOption(ROOM_SELECT, 'desk');
  await wait(300);
  is('choosing the desk for this room plays it there', await playing(page), 'desk');
  is('and is written down against this room', await stored(page), { Lobby: 'desk' });
  await closeSettings(page);

  await goTo(page, 'Studio');
  is('another room plays through the door\'s', await playing(page), 'headset');
  await openSettings(page);
  is('and has nothing of its own', await page.inputValue(ROOM_SELECT), '');
  await closeSettings(page);

  await goTo(page, 'Lobby');
  is('walking back in plays through the desk again', await playing(page), 'desk');

  // Another day: the door again, and the same choice already made.
  await page.reload();
  await comeIn(page);
  if ((await whereYouAre(page)) !== 'Lobby') await goTo(page, 'Lobby');
  is('a room remembers its speakers across a reload', await playing(page), 'desk');

  await openSettings(page);
  is('and Settings says so', await page.inputValue(ROOM_SELECT), 'desk');
  await plugDesk(page, false);
  is('desk unplugged: back to the door\'s', await playing(page), 'headset');
  is('but the choice is kept', await page.inputValue(ROOM_SELECT), 'desk');
  is('and the page says why the sound moved',
    (await page.locator('.settings .speaker + .hint').textContent()).includes('aren\'t plugged in'),
    true);
  await plugDesk(page, true);
  is('plugged back in: the desk again', await playing(page), 'desk');

  await page.selectOption(ROOM_SELECT, '');
  await wait(300);
  is('back to the door\'s speakers', await playing(page), 'headset');
  is('and nothing is left written down', await stored(page), {});

  await browser.close();
  report();
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
