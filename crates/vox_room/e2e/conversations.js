// A room keeps the conversations it has had, and they can be named,
// read, downloaded and deleted:
//   node conversations.js RYAN_LINK ALEX_LINK
//
// **New conversation** closes the one you were having; the old one goes
// into the room's history, where it is read-only and yours alone to
// open — nobody else's screen moves. Deleting one is the exception and
// takes it from everybody. A conversation nobody spoke in is not one
// the room had. And what each is called is a box at the head of it,
// defaulting to when it was.
//
//   vox_room --room Lobby --room Studio --echo --no-stt --tts none

const { is, report, person, said, type } = require('./room.js');
const [ryanLink, alexLink] = process.argv.slice(2);

const name = (page) => page.evaluate(() => document.querySelector('.bar input.name')?.value ?? null);
const canType = (page) => page.evaluate(() => !!document.querySelector('.say'));
const rows = (page) =>
  page.evaluate(() =>
    [...document.querySelectorAll('.menu .row')].map((r) => ({
      name: r.querySelector('.at').textContent.trim(),
      head: r.querySelector('.head').textContent.replace(/\s+/g, ' ').trim(),
      opening: r.querySelector('.opening').textContent.trim(),
      deletable: !!r.querySelector('.drop'),
    })),
  );

const openHistory = async (page) => {
  await page.click('.picker > button');
  await page.waitForTimeout(800);
};
const shut = async (page) => {
  await page.keyboard.press('Escape');
  await page.waitForTimeout(300);
};

(async () => {
  const ryan = await person(ryanLink);
  const alex = await person(alexLink);

  // Nothing said yet: the conversation is called the day it started.
  const blank = await name(ryan.page);
  is('an empty conversation is called the day it started',
    /^[A-Z][a-z]{2} \d{1,2}, \d{4}$/.test(blank ?? ''), true);

  // And pressing New conversation on it leaves nothing behind.
  for (let i = 0; i < 2; i++) {
    await ryan.page.click('.bar button:has-text("New conversation")');
    await ryan.page.waitForTimeout(300);
    await ryan.page.click('dialog.ask button.primary');
    await ryan.page.waitForTimeout(1400);
  }
  await openHistory(ryan.page);
  is('a conversation nobody spoke in is not one the room had', (await rows(ryan.page)).length, 1);
  is('but the one you are in is on the list',
    /now/.test((await rows(ryan.page))[0].head), true);
  await shut(ryan.page);

  // Say something and the name follows the first thing said.
  await type(ryan.page, 'planning the heist');
  is('once something is said, it is called when that was',
    /^[A-Z][a-z]{2} \d{1,2}, \d{2}:\d{2}\s?[AP]M$/.test((await name(ryan.page)) ?? ''), true);

  // Call it something: everyone in the room sees it.
  await ryan.page.fill('.bar input.name', 'The heist');
  await ryan.page.keyboard.press('Enter');
  await ryan.page.waitForTimeout(900);
  is('named', await name(ryan.page), 'The heist');
  is('and Alex sees the name too', await name(alex.page), 'The heist');

  // A new conversation is blank, and Alex is brought into it too.
  await ryan.page.click('.bar button:has-text("New conversation")');
  await ryan.page.waitForTimeout(300);
  await ryan.page.click('dialog.ask button.primary');
  await ryan.page.waitForTimeout(1600);
  is('a blank thread', await said(ryan.page), []);
  is('for everyone in the room', await said(alex.page), []);
  is('and unnamed again', /^[A-Z][a-z]{2} /.test((await name(ryan.page)) ?? ''), true);
  await type(ryan.page, 'and afterwards');

  // The history has both, newest first, and the old one kept its name.
  await openHistory(ryan.page);
  const list = await rows(ryan.page);
  is('two conversations', list.length, 2);
  is('the live one is marked', /now/.test(list[0].head), true);
  is('and is not an archive anybody can delete', list[0].deletable, false);
  is('the older one can be', list[1].deletable, true);
  is('and kept the name it was given', list[1].name, 'The heist');
  is('with what was said in it', /planning the heist/.test(list[1].opening), true);

  // Open it: read-only, and nobody else moves.
  await ryan.page.click('.menu .row:last-child .item');
  await ryan.page.waitForTimeout(900);
  is('reading the old one', (await said(ryan.page)).some((t) => /planning the heist/.test(t)), true);
  is('there is nothing to type into', await canType(ryan.page), false);
  is('the box says which one you are reading', await name(ryan.page), 'The heist');
  is('and it says it is shut',
    await ryan.page.evaluate(() =>
      /closed, nothing can be said into it$/.test(
        document.querySelector('.bar .closed')?.textContent.replace(/\s+/g, ' ').trim() ?? '',
      )), true);

  // An old one can be named too: read-only means nothing can be *said*
  // into it, and a name is not something said.
  const hers = await name(alex.page);
  await ryan.page.fill('.bar input.name', 'The heist, properly');
  await ryan.page.keyboard.press('Enter');
  await ryan.page.waitForTimeout(900);
  is('an old one can be named', await name(ryan.page), 'The heist, properly');
  is('and naming it left the live one alone', await name(alex.page), hers);
  is("Alex's screen never moved",
    (await said(alex.page)).some((t) => /afterwards/.test(t)), true);
  is('and she can still talk', await canType(alex.page), true);

  await ryan.page.click('.bar button.back');
  await ryan.page.waitForTimeout(600);
  is('back in the live one', (await said(ryan.page)).some((t) => /afterwards/.test(t)), true);
  is('and can type again', await canType(ryan.page), true);

  // And one can be taken away. Fetched rather than clicked: what the
  // link does is hand the browser a download, and the question here is
  // what is in it -- which is also the one way to see that the server
  // assembled it at all.
  await openHistory(ryan.page);
  const file = await ryan.page.evaluate(async () => {
    const href = document.querySelector('.menu .row:last-child .save')?.getAttribute('href');
    if (!href) return null;
    const r = await fetch(href);
    return { ok: r.ok, as: r.headers.get('content-disposition'), md: await r.text() };
  });
  is('a conversation can be downloaded', file?.ok, true);
  is('as a file named after the room, the conversation and the day',
    /^attachment; filename="lobby-\d+-\d{4}-\d{2}-\d{2}-transcript\.md"$/.test(file?.as ?? ''), true);
  is('under the name it was given', /^# The heist, properly$/m.test(file?.md ?? ''), true);
  is('with who said what, and when they said it',
    /\*\*\d{2}:\d{2} · Ryan\*\* \(typed\) — planning the heist$/m.test(file?.md ?? ''), true);
  is('and the clock the times are in',
    /times are UTC([+-]\d{2}:\d{2})?\./.test(file?.md ?? ''), true);
  await shut(ryan.page);

  // Deleting takes it from everybody, and for good.
  await openHistory(ryan.page);
  await ryan.page.click('.menu .row:last-child .drop');
  await ryan.page.waitForTimeout(400);
  await ryan.page.click('dialog.ask button.primary');
  await ryan.page.waitForTimeout(900);
  is('one left', (await rows(ryan.page)).length, 1);
  is('and the list stayed open to delete from', await ryan.page.evaluate(() => !!document.querySelector('.menu')), true);

  await ryan.page.reload();
  await ryan.page.locator('.consent input').check();
  await ryan.page.click('button:has-text("Join Audio")');
  await ryan.page.waitForTimeout(2500);
  await openHistory(ryan.page);
  is('still one after a reload, so it is gone from the server', (await rows(ryan.page)).length, 1);

  // -- purging the whole room -------------------------------------------

  // Say something else, so there are two again and one of them is the
  // one being sat in.
  await shut(ryan.page);
  await type(ryan.page, 'and one more thing');
  await ryan.page.click('.bar button:has-text("New conversation")');
  await ryan.page.waitForTimeout(300);
  await ryan.page.click('dialog.ask button.primary');
  await ryan.page.waitForTimeout(1400);
  await type(ryan.page, 'the one we are in');
  await openHistory(ryan.page);
  is('two conversations to purge', (await rows(ryan.page)).length, 2);

  // The owner's alone — Alex is in the same room and has no button.
  is('only the owner is offered it',
    await alex.page.evaluate(() => !!document.querySelector('.menu .purge')), false);

  // Asked twice over: a question, and the room's name written out,
  // because one click is too cheap a way to lose every conversation.
  await ryan.page.click('.menu .purge');
  await ryan.page.waitForSelector('dialog.ask[open]');
  is('the question names the room and says the live one goes too',
    /Purge Lobby\?.*The one you are in now goes too/s.test(await ryan.page.textContent('dialog.ask p')), true);
  is('and it will not take yes for an answer yet',
    await ryan.page.evaluate(() => document.querySelector('dialog.ask button.primary')?.disabled), true);
  await ryan.page.fill('dialog.ask input.confirm', 'Lounge');
  is('nor the wrong name',
    await ryan.page.evaluate(() => document.querySelector('dialog.ask button.primary')?.disabled), true);
  await ryan.page.screenshot({ path: '/tmp/vox-room-purge.png' });
  await ryan.page.fill('dialog.ask input.confirm', 'Lobby');
  await ryan.page.waitForTimeout(200);
  is('the room\'s own name is the key',
    await ryan.page.evaluate(() => document.querySelector('dialog.ask button.primary')?.disabled), false);
  await ryan.page.click('dialog.ask button.primary');
  await ryan.page.waitForTimeout(1600);

  // Everything went, including the one he was in — and the room is
  // left somewhere to be rather than staring at a conversation that
  // is no longer there.
  is('nothing is left on screen', await said(ryan.page), []);
  is('and everyone in the room is in the fresh one', await said(alex.page), []);
  // The list is still open, as it is after deleting one, and it has
  // re-read itself: what is in it now is the conversation the room
  // started in the same breath.
  await ryan.page.waitForTimeout(600);
  is('the history is the one the room is having now', (await rows(ryan.page)).length, 1);
  is('which is the live one', /now/.test((await rows(ryan.page))[0].head), true);
  is('and it has nothing in it', (await rows(ryan.page))[0].opening, 'Nothing was said.');
  await shut(ryan.page);

  await ryan.page.reload();
  await ryan.page.locator('.consent input').check();
  await ryan.page.click('button:has-text("Join Audio")');
  await ryan.page.waitForTimeout(2500);
  await openHistory(ryan.page);
  is('still one after a reload, so the room really is empty',
    (await rows(ryan.page)).length, 1);

  await ryan.browser.close();
  await alex.browser.close();
  report();
})();
