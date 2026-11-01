// A room keeps the conversations it has had, and they can be named,
// read and deleted:
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
const canType = (page) => page.evaluate(() => !!document.querySelector('.composer'));
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
    await ryan.page.click('header button:has-text("New conversation")');
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
  await ryan.page.click('header button:has-text("New conversation")');
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

  await ryan.browser.close();
  await alex.browser.close();
  report();
})();
