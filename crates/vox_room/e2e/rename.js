// Taking a new name needs a new socket (see `reconnect`), and the socket
// is the worker's now. One person renames themselves in a browser;
// somebody else watching on a plain socket says whether the room still
// has them, and what they do next says whether the new socket carries
// anything -- a change of room is a command out and a `hello` back, so
// it is both halves at once.
//
//   e2e/run.sh rename
const { person, socket, until, is, report, goTo, whereYouAre } = require('./room.js');
const [ryanLink, alexLink] = process.argv.slice(2);
const token = (l) => l.replace(/.*\/join\//, '');

(async () => {
  // Alex only watches, from the lobby: who is on his roster is who is in
  // the room with him.
  const alex = await socket(token(alexLink));
  const ryan = await person(ryanLink);
  await until(() => alex.people.includes('Ryan'));
  is('the room has Ryan', alex.people.includes('Ryan'), true);

  // Rename from Settings, which the cog opens: the box saves when it
  // loses the focus.
  await ryan.page.click('.tools button.cog');
  await ryan.page.waitForSelector('.settings');
  await ryan.page.waitForSelector('.name input');
  await ryan.page.fill('.name input', 'Rupert');
  await ryan.page.locator('.name input').blur();
  await until(() => alex.people.includes('Rupert'));
  is('and calls him Rupert when he asks', alex.people.includes('Rupert'), true);
  is('under one name and not two', alex.people.includes('Ryan'), false);

  // Out of the settings and off to the other room. The click is a
  // command on whatever socket he is on now; the heading changing is
  // the `hello` that came back on it.
  await ryan.page.keyboard.press('Escape');
  await goTo(ryan.page, 'Studio');
  is('his new socket takes him to another room', await whereYouAre(ryan.page), 'Studio');
  await until(() => !alex.people.includes('Rupert'));
  is('and the lobby he left no longer has him', alex.people.includes('Rupert'), false);

  await ryan.browser.close();
  alex.ws.close();
  report();
})();
