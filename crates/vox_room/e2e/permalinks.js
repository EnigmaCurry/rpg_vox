// Every room has an address, and the bar follows you:
//   node permalinks.js RYAN_LINK
//
// `/` is the room of your own, `/lobby` the first room named with
// --room, `/room/<id>` the rest. Back and forward move between rooms,
// a pasted address opens that room, and one that names no room is put
// quietly right rather than left broken.
//
// Wants a room started with at least two shared rooms, the first of
// them the hall a tab lands in:
//   vox_room --room Hall --room Lobby --room Studio --echo --no-stt --tts none
//
// Nothing here is said in a room, so this one stays in the hall it
// lands in -- where that is is half of what it is about. `run.sh` says
// which suites do; see `room()` there.

const { BASE, JOIN, AGREE, is, report, person, whereYouAre, goTo } = require('./room.js');
const [link] = process.argv.slice(2);

(async () => {
  const { browser, ctx, page } = await person(link);
  const path = () => new URL(page.url()).pathname;

  // An invite comes in to be with people, not to be alone with the bot:
  // it lands in the lobby, whatever that room is called.
  is('an invite landed in the lobby', path(), '/lobby');
  is('and that is where it is', await whereYouAre(page), 'Hall');

  await goTo(page, 'Studio', 900);
  const studio = path();
  is('a room has an address of its own', /^\/room\/[0-9a-f]{32}$/.test(studio), true);
  is('and you are in it', await whereYouAre(page), 'Studio');

  await goTo(page, 'You and Vox', 900);
  is('your own room is the root', path(), '/');
  is('and you are in it', await whereYouAre(page), 'You and Vox');

  // Back and forward move between rooms, because that is what the bar
  // changing means.
  await page.goBack();
  await page.waitForTimeout(900);
  is('back goes to the room before', path(), studio);
  is('and really moves you', await whereYouAre(page), 'Studio');
  await page.goForward();
  await page.waitForTimeout(900);
  is('forward comes back', await whereYouAre(page), 'You and Vox');

  // A pasted address opens the door first — a tab at the door is not in
  // the room — and is honoured the moment it comes in.
  const fresh = await ctx.newPage();
  await fresh.goto(BASE + studio);
  await fresh.waitForSelector('.enter');
  is('an address opens the door, not the room',
    await fresh.evaluate(() => !document.querySelector('.app')), true);
  await fresh.locator(AGREE).check();
  await fresh.click(JOIN);
  await fresh.waitForTimeout(2800);
  is('and coming in lands in the room it named', await whereYouAre(fresh), 'Studio');
  is('with the address kept', new URL(fresh.url()).pathname, studio);
  await fresh.close();

  // One that names no room is put right rather than left broken.
  const stale = await ctx.newPage();
  await stale.goto(`${BASE}/room/deadbeefdeadbeefdeadbeefdeadbeef`);
  await stale.waitForSelector('.enter');
  await stale.locator(AGREE).check();
  await stale.click(JOIN);
  await stale.waitForTimeout(2800);
  is('a stale address becomes the one you ended up in', new URL(stale.url()).pathname, '/lobby');
  await stale.close();

  await browser.close();
  report();
})();
