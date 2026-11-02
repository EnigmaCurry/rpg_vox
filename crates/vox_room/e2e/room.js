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

/// Somebody at a link, through the door and in the room.
async function person(url, { wav, locale, capture, viewport, quiet = false, settle } = {}) {
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
      if (m.type() === 'error') console.log('[console]', m.text());
    });
  }
  await page.goto(url);
  await comeIn(page, settle === undefined ? {} : { settle });
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

/// The names on the strips under the room you are in.
const strips = (page) =>
  page.evaluate(() =>
    [...document.querySelectorAll('.room.here .strip .label')].map((e) =>
      e.textContent.trim().replace(/\s+/g, ' '),
    ),
  );

/// Which room the list says you are in.
const whereYouAre = (page) =>
  page.evaluate(() => document.querySelector('.rooms [aria-checked=true] .name')?.textContent.trim());

/// Everything on screen that somebody said.
const said = (page) =>
  page.evaluate(() =>
    [...document.querySelectorAll('.entry .text')].map((e) => e.textContent.trim()).filter(Boolean),
  );

/// Type something into the room instead of saying it, and give the AI
/// its pause to answer in.
async function type(page, text, settle = 2600) {
  await page.fill('.composer input', text);
  await page.keyboard.press('Enter');
  await page.waitForTimeout(settle);
}

// -- a participant with no browser at all ----------------------------

/// Spend an invite link for a cookie. The order matters: the link is
/// only remembered until `/api/door/join`, which is what spends it.
async function cookieFor(token, base = BASE) {
  let r = await fetch(`${base}/join/${token}`, { redirect: 'manual' });
  const pending = r.headers.getSetCookie().join('; ');
  r = await fetch(`${base}/api/door/join`, { method: 'POST', headers: { cookie: pending } });
  if (!r.ok) throw new Error(`door/join ${r.status}: ${await r.text()}`);
  return r.headers
    .getSetCookie()
    .map((c) => c.split(';')[0])
    .join('; ');
}

/// Somebody in the room on nothing but a socket: no browser, no audio,
/// no WebCodecs. Enough for everything that is room state rather than
/// something you have to hear or look at.
async function socket(token, base = BASE) {
  const cookie = await cookieFor(token, base);
  const ws = new WebSocket(`${base.replace('http', 'ws')}/room.ws`, { headers: { cookie } });
  ws.binaryType = 'arraybuffer';
  const p = { ws, rooms: [], notices: [], frames: 0, seen: '', people: [], said: [] };
  p.send = (m) => ws.send(JSON.stringify(m));
  ws.onmessage = (ev) => {
    if (typeof ev.data !== 'string') {
      p.frames++;
      return;
    }
    const m = JSON.parse(ev.data);
    if (m.type === 'hello') {
      p.where = m.room;
      p.people = m.people.map((q) => q.name);
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
      p.speaks = m.speaks;
    } else if (m.type === 'mode') {
      const bot = (p.agents ?? []).find((b) => b.name === m.bot);
      if (bot) bot.mode = m.mode;
      if (p.agents?.[0]) p.mode = p.agents[0].mode;
    } else if (m.type === 'agents') {
      for (const a of m.agents) {
        const bot = (p.agents ?? []).find((b) => b.name === a.name);
        if (bot) bot.state = a.state;
      }
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
    }
  };
  ws.onerror = (e) => console.log('[socket]', e.message);
  await new Promise((ok) => (ws.onopen = ok));
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
  said,
  type,
  cookieFor,
  socket,
};
