// What every end-to-end script here needs: a way through the door, a
// browser whose microphone is a file, and a tally of what passed.
//
// The door is three requests and a tick. An invite link is spent by
// **Join Audio**, so every run wants fresh links; and since nobody comes
// in without agreeing to be recorded, every script has to tick the box
// before it presses the button. `run.sh` mints the links and starts a
// room to point these at.
//
// Playwright is loaded only when a browser is actually asked for, so the
// scripts that need no browser (see `socket`) run without it.

const BASE = process.env.VOX_ROOM_URL || 'http://127.0.0.1:7340';

/// The room a script is about, which `run.sh` names in `room()`.
///
/// A tab lands in the lobby, and the lobby is never written down and
/// has no AI in it: it is the room anybody can walk into and the room
/// you wait in (see ROOM.md, **Off the record**). So a suite about
/// anything said in a room is started with a hall to land in and a
/// room after it to be about, and `person` and `socket` walk out of
/// the one into the other -- which is what a person does.
///
/// Unset is a suite that wants the lobby itself and stays where it
/// lands. `run.sh` is the only place that is decided; `room:` here is
/// for a second person in a script who belongs somewhere else.
const ROOM = process.env.VOX_ROOM_E2E_ROOM || null;

const wait = (ms) => new Promise((r) => setTimeout(r, ms));

/// Wait until something is so. Room state arrives when the room has
/// something to say, so a script waits on what it is actually after
/// rather than on a number of seconds it guessed.
async function until(ok, { tries = 80, every = 150 } = {}) {
  for (let i = 0; i < tries; i++) {
    if (ok()) return true;
    await wait(every);
  }
  return false;
}

// The button, not the card's prose about it.
const JOIN = 'button:has-text("Join Audio")';
// What you agree to before it does anything.
const AGREE = '.consent input';

// -- what passed -----------------------------------------------------

let failed = 0;

/// One thing that should be so. Prints it either way, because a script
/// that only speaks up when it is unhappy is one you can't read.
function is(what, got, want) {
  const ok = JSON.stringify(got) === JSON.stringify(want);
  if (!ok) failed++;
  const said = JSON.stringify(got);
  console.log(
    `${ok ? 'ok  ' : 'FAIL'} ${what}: ${said}${ok ? '' : ` (wanted ${JSON.stringify(want)})`}`,
  );
  return ok;
}

/// A page refusing to do something because of its own policy.
///
/// Never acceptable and never only cosmetic: the room sets a strict
/// `Content-Security-Policy` (see `policy()` in web.rs) and a policy
/// that is a shade too strict breaks a feature quietly, in one browser,
/// with nothing on the screen saying why. Every suite watches for it,
/// so whichever one touches the broken thing is the one that says so.
function violation(text) {
  failed++;
  console.log(`FAIL the page was refused by its own policy: ${text}`);
}

/// Say how it went and leave with an exit code that says the same.
function report() {
  console.log(failed ? `\n${failed} failed` : '\nall ok');
  process.exit(failed ? 1 : 0);
}

// -- a browser that can talk -----------------------------------------

/// A headless Chromium with a fake microphone. `wav` makes that
/// microphone a file; without one it is Chromium's own test tone, which
/// is enough to move a meter and not enough to transcribe.
///
/// Playwright's own headless shell has no screen capture in it, so a
/// script that shares a desktop needs a whole Chromium: set CHROME.
async function launch({ wav, capture = false } = {}) {
  const { chromium } = require('playwright');
  const path = require('path');
  return chromium.launch({
    executablePath: process.env.CHROME || undefined,
    channel: process.env.CHROME ? undefined : 'chromium',
    args: [
      '--use-fake-ui-for-media-stream',
      '--use-fake-device-for-media-stream',
      ...(wav ? [`--use-file-for-fake-audio-capture=${path.resolve(wav)}%noloop`] : []),
      ...(capture ? ['--auto-accept-this-tab-capture'] : []),
      '--autoplay-policy=no-user-gesture-required',
    ],
  });
}

/// Tick the box and press the button. Everything behind the door waits
/// on this: until the audio is joined there is no socket, no roster and
/// no conversation, which is the point of the door.
async function comeIn(page, { settle = 2200 } = {}) {
  await page.waitForSelector(AGREE);
  await page.locator(AGREE).check();
  await page.click(JOIN);
  await page.waitForTimeout(settle);
}

/// Somebody at a link, through the door and in the room the suite is
/// about -- see [`ROOM`]. `room: null` stays where it landed.
async function person(url, { wav, locale, capture, viewport, quiet = false, settle, room = ROOM } = {}) {
  const browser = await launch({ wav, capture });
  const ctx = await browser.newContext({
    permissions: ['microphone'],
    ...(locale ? { locale } : {}),
    viewport: viewport ?? { width: 1280, height: 860 },
  });
  const page = await ctx.newPage();
  if (!quiet) {
    page.on('pageerror', (e) => console.log('[pageerror]', e.message));
    page.on('console', (m) => {
      if (m.type() !== 'error') return;
      if (/Content Security Policy/i.test(m.text())) return violation(m.text());
      console.log('[console]', m.text());
    });
  }
  await page.goto(url);
  await comeIn(page, settle === undefined ? {} : { settle });
  if (room) await goTo(page, room);
  return { browser, ctx, page };
}

/// At the door and no further, for the scripts about the door itself.
async function atTheDoor(url, opts = {}) {
  const browser = await launch(opts);
  const ctx = await browser.newContext({
    permissions: ['microphone'],
    ...(opts.locale ? { locale: opts.locale } : {}),
    viewport: opts.viewport ?? { width: 900, height: 1000 },
  });
  const page = await ctx.newPage();
  page.on('pageerror', (e) => console.log('[pageerror]', e.message));
  await page.goto(url);
  await page.waitForSelector('.enter');
  await page.waitForTimeout(opts.settle ?? 1200);
  return { browser, ctx, page };
}

// -- reading the room off a page -------------------------------------

/// The names on the strips for the room you are in. They are under the
/// heading that names it, and they are the only strips on the page.
const strips = (page) =>
  page.evaluate(() =>
    [...document.querySelectorAll('.rooms .strip .label')].map((e) =>
      e.textContent.trim().replace(/\s+/g, ' '),
    ),
  );

/// Which room the heading says you are in.
const whereYouAre = (page) =>
  page.evaluate(() => document.querySelector('.pick .current .name')?.textContent.trim());

/// Open the heading, which is the only way to see the rooms: the list
/// is behind it and over the strips.
async function openRooms(page) {
  await page.click('.pick .current');
  await page.waitForSelector('.pick .list');
}

async function closeRooms(page) {
  await page.keyboard.press('Escape');
  await page.waitForTimeout(200);
}

/// Every room you may go to, as the picker lists them -- the whole of
/// each row, so a game being played in one is in what comes back.
async function roomsListed(page) {
  await openRooms(page);
  const rows = await page.evaluate(() =>
    [...document.querySelectorAll('.pick .list button')].map((b) =>
      b.textContent.trim().replace(/\s+/g, ' '),
    ),
  );
  await closeRooms(page);
  return rows;
}

/// Out of the lobby and into the first room of the house after it.
///
/// A tab lands in the lobby, which is never written down and has no AI
/// in it -- it is the room anybody can walk into, and the room you
/// wait in (see ROOM.md, **Off the record**). The scripts that are run
/// by hand against a room somebody started themselves have no name to
/// walk to, so they walk to the first room after the lobby: the picker
/// lists your own room, then the house's in the order they were named.
/// A house that is nothing but a lobby leaves them where they are, and
/// says so rather than waiting for a room that isn't there.
async function intoARoom(page, settle = 1400) {
  await page.click('.pick .current');
  await page.waitForSelector('.pick .list');
  const rooms = await page.evaluate(() =>
    [...document.querySelectorAll('.pick .list button .name')].map((e) => e.textContent.trim()),
  );
  // Your own room, then the lobby, then the rooms of the house.
  const want = rooms[2];
  if (!want) {
    await closeRooms(page);
    console.log('[room] this house is nothing but a lobby, where nothing is written down');
    return null;
  }
  await page.click('.pick .list button:nth-of-type(3)');
  await page.waitForTimeout(settle);
  return want;
}

/// Go to another room: open the heading, press the room, and let the
/// new one arrive. The list shuts itself on the way.
async function goTo(page, title, settle = 1200) {
  await page.click('.pick .current');
  await page.waitForSelector('.pick .list');
  await page.click(`.pick .list button:has-text("${title}")`);
  await page.waitForTimeout(settle);
}

/// Everything on screen that somebody said.
const said = (page) =>
  page.evaluate(() =>
    [...document.querySelectorAll('.entry .text')].map((e) => e.textContent.trim()).filter(Boolean),
  );

/// Type something into the room instead of saying it, and give the AI
/// its pause to answer in.
async function type(page, text, settle = 2600) {
  await page.fill('.say textarea', text);
  await page.keyboard.press('Enter');
  await page.waitForTimeout(settle);
}

// -- a participant with no browser at all ----------------------------

/// Spend an invite link for a cookie. One request: the token goes in
/// the body, because the room hands a browser nothing until this is
/// called -- opening `/join/<token>` is served the app and sets no
/// cookie at all, so there is nothing to carry from it.
async function cookieFor(token, base = BASE) {
  const r = await fetch(`${base}/api/door/join`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ token }),
  });
  if (!r.ok) throw new Error(`door/join ${r.status}: ${await r.text()}`);
  return r.headers
    .getSetCookie()
    .map((c) => c.split(';')[0])
    .join('; ');
}

/// Somebody in the room on nothing but a socket: no browser, no audio,
/// no WebCodecs. Enough for everything that is room state rather than
/// something you have to hear or look at.
async function socket(token, { base = BASE, room = ROOM } = {}) {
  const cookie = await cookieFor(token, base);
  const ws = new WebSocket(`${base.replace('http', 'ws')}/room.ws`, { headers: { cookie } });
  ws.binaryType = 'arraybuffer';
  const p = { ws, cookie, rooms: [], notices: [], frames: 0, seen: '', people: [], roster: [], said: [] };
  p.send = (m) => ws.send(JSON.stringify(m));
  ws.onmessage = (ev) => {
    if (typeof ev.data !== 'string') {
      p.frames++;
      return;
    }
    const m = JSON.parse(ev.data);
    if (m.type === 'hello') {
      p.where = m.room;
      // Whether this room is written down, and whether that is a thing
      // it can be asked about at all: the lobby is never written down
      // and has nothing to turn on.
      p.transcribing = m.transcribing;
      p.mayTranscribe = m.may_transcribe;
      p.people = m.people.map((q) => q.name);
      // The roster itself, for what it says about each of them:
      // whether their mic is open, and whether the AIs may answer
      // them, which is each person's own switch.
      p.roster = m.people;
      // The AIs in the room, each with its own name, fader and say in
      // when it speaks. `mode` is the first one's, which is the room's
      // when there is only one of them in it.
      p.agents = m.agents;
      p.mode = m.agents[0]?.mode;
      p.strips = Object.keys(m.strips);
      // The levels themselves, for the scripts that move a fader and
      // want to know whose it was.
      p.faders = m.strips;
      p.log = m.log.map((e) => `${e.who}: ${e.text}`);
      p.conversation = m.conversation;
    } else if (m.type === 'people') {
      p.people = m.people.map((q) => q.name);
      p.roster = m.people;
      p.speaks = m.speaks;
    } else if (m.type === 'mode') {
      const bot = (p.agents ?? []).find((b) => b.name === m.bot);
      if (bot) bot.mode = m.mode;
      if (p.agents?.[0]) p.mode = p.agents[0].mode;
    } else if (m.type === 'stops') {
      const bot = (p.agents ?? []).find((b) => b.name === m.bot);
      if (bot) bot.stops = m.stops;
    } else if (m.type === 'agents') {
      // Names and states only: the light one, sent many times a turn.
      for (const a of m.agents) {
        const bot = (p.agents ?? []).find((b) => b.name === a.name);
        if (bot) bot.state = a.state;
      }
    } else if (m.type === 'voice') {
      // Which voice one of them is speaking in. A browser follows this
      // to keep the name on the strip right, and so must a socket:
      // without it the voice on `agents` is whatever it was when the
      // room was last described in full.
      const bot = (p.agents ?? []).find((b) => b.name === m.bot);
      if (bot) bot.voice = m.voice ?? null;
    } else if (m.type === 'agents_here') {
      // And the heavy one: which voices are in the room and everything
      // set on each. A browser has always followed this; a socket used
      // not to, so anything *set* on a bot while a script was watching
      // -- which voice it speaks in, whether it is switched off -- was
      // a change the script never saw.
      p.agents = m.agents;
      p.mode = m.agents[0]?.mode;
    } else if (m.type === 'rooms') {
      p.rooms = m.rooms;
    } else if (m.type === 'notice') {
      p.notices.push(m.text);
    } else if (m.type === 'entry') {
      p.last = `${m.entry.who}: ${m.entry.text}`;
      p.seen += ` ${m.entry.text}`;
      p.said.push(m.entry);
    } else if (m.type === 'entry_removed') {
      // A turn that was passed on, or taken back before it had any
      // words in it: it leaves nothing on anybody's screen, so it must
      // leave nothing here either.
      p.said = p.said.filter((e) => e.id !== m.id);
    } else if (m.type === 'transcribe') {
      // Somebody put the room off the record, or back on it. The note
      // saying who arrives as an entry like any other, and the AIs
      // leaving or coming back as `agents_here`.
      p.transcribing = m.on;
    }
  };
  ws.onerror = (e) => console.log('[socket]', e.message);
  await new Promise((ok) => (ws.onopen = ok));
  // Out of the hall and into the room this suite is about, which is
  // what the person whose socket this is would have done.
  if (room) {
    await until(() => !!p.where);
    if (p.where.id !== room) {
      p.send({ type: 'enter_room', id: room });
      await until(() => p.where?.id === room);
    }
  }
  return p;
}

module.exports = {
  BASE,
  JOIN,
  AGREE,
  wait,
  until,
  is,
  report,
  launch,
  comeIn,
  person,
  atTheDoor,
  strips,
  whereYouAre,
  openRooms,
  closeRooms,
  roomsListed,
  intoARoom,
  goTo,
  said,
  type,
  cookieFor,
  socket,
};
