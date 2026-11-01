// A game is something a room does, and leaving it puts the room back:
//   node game.js RYAN_LINK ALEX_LINK
//
// It is played in a room, by the people in that room, and while it runs
// the game is what that room is. Leaving it picks up the conversation
// the room was having — every word of it, for everyone in the room at
// once. Needs a saved game to play; `run.sh` plants one.
//
//   vox_room --room Lobby --room Studio --echo --no-stt --tts none

const { is, report, person, said, type, whereYouAre } = require('./room.js');
const [ryanLink, alexLink] = process.argv.slice(2);

const playing = (page) =>
  page.evaluate(() => document.querySelector('header .playing')?.textContent.trim() ?? null);
const inList = (page) =>
  page.evaluate(() =>
    [...document.querySelectorAll('.rooms .room')].map((r) =>
      r.querySelector('button').textContent.trim().replace(/\s+/g, ' '),
    ),
  );

(async () => {
  const ryan = await person(ryanLink);
  const alex = await person(alexLink);

  await type(ryan.page, 'before the game');
  is('something was said in the room',
    (await said(ryan.page)).some((t) => /before the game/.test(t)), true);
  is('no game yet', await playing(ryan.page), null);

  // A game is played from its card in the conversation that announced
  // it; there is no menu of them in the top bar.
  is('the card offers it', await ryan.page.evaluate(() =>
    [...document.querySelectorAll('.game-entry button')].map((b) => b.textContent.trim())),
    ['Play', 'Peek']);
  await ryan.page.click('.game-entry button:has-text("Play")');
  await ryan.page.waitForTimeout(400);
  await ryan.page.click('dialog.ask button.primary');
  await ryan.page.waitForTimeout(2500);

  is('the room is playing it', await playing(ryan.page), 'Quick Draw');
  is('and still says which room it is', await whereYouAre(ryan.page), 'Lobby');
  is('the game took the room over',
    (await said(ryan.page)).some((t) => /before the game/.test(t)), false);
  is('Alex is in the game too', await playing(alex.page), 'Quick Draw');
  is('and the room list says which room has one on',
    (await inList(alex.page)).some((r) => /Lobby.*Quick Draw/.test(r)), true);

  // Out of the game: the room is where it was, for both of them.
  await ryan.page.click('header button:has-text("Leave the game")');
  await ryan.page.waitForTimeout(400);
  await ryan.page.click('dialog.ask button.primary');
  await ryan.page.waitForTimeout(2500);

  is('the game is over', await playing(ryan.page), null);
  is('and the room is back where it was',
    (await said(ryan.page)).some((t) => /before the game/.test(t)), true);
  is('for Alex as well',
    (await said(alex.page)).some((t) => /before the game/.test(t)), true);
  is('and the list is plain again',
    (await inList(alex.page)).some((r) => /Quick Draw/.test(r)), false);

  await ryan.browser.close();
  await alex.browser.close();
  report();
})();
