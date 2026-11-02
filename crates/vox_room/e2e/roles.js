// Who owns the house, and who may go where:
//   node roles.js OWNER_LINK ALEX_LINK
//
// The owner is made at the server and nowhere else, and is the only one
// who can see or change the people. Everybody else has the lobby and a
// room of their own; every other room is given to them one at a time,
// and until it is, it is neither on their list nor behind its address.
//
//   vox_room --room Lobby --room Studio --room Cellar --echo --no-stt --tts none
//   vox_room invite-owner Ryan   # the first link
//   vox_room invite Alex         # the second

const { BASE, is, report, person, socket, until, wait } = require('./room.js');
const [ownerLink, alexLink] = process.argv.slice(2);
const alexToken = alexLink.replace(/.*\/join\//, '');

/// The rooms a socket is shown, by id.
const listed = (p) => p.rooms.map((r) => r.id);

/// Ask the people API with somebody's cookie and say what came back.
const asked = async (cookie, path, init = {}) => {
  const r = await fetch(BASE + path, { ...init, headers: { cookie, ...(init.headers ?? {}) } });
  return { status: r.status, body: await r.json().catch(() => null) };
};

(async () => {
  // Alex, on a socket, so what she is shown can be watched as it
  // changes under her.
  const alex = await socket(alexToken);
  await wait(400);
  is('a plain user has the lobby and a room of her own', listed(alex), ['@Alex', 'Lobby']);

  alex.send({ type: 'enter_room', id: 'Studio' });
  await wait(600);
  is('and cannot walk into a room she has not been given', alex.where.id, 'Lobby');
  is('told only that it is not here', alex.notices, ["That room isn't here."]);

  // The owner, in a browser, because the page is the thing being
  // tested. The owner sees every room without being given one.
  const boss = await person(ownerLink);
  is('the owner has the house',
    await boss.page.evaluate(() =>
      [...document.querySelectorAll('.rooms .room > button .name')].map((e) => e.textContent.trim())),
    ['You and Vox', 'Lobby', 'Studio', 'Cellar']);

  // Only the owner is offered the page at all.
  await boss.page.click('footer button.cog');
  await boss.page.waitForSelector('.settings');
  is('and is offered the people', await boss.page.evaluate(() => !!document.querySelector('.settings button.manage')), true);
  await boss.page.click('.settings button.manage');
  // The page asks the room before it has anything to show, so wait for
  // what came back rather than for the page.
  await boss.page.waitForSelector('.people tbody tr');

  const row = (page, who) =>
    page.evaluate((n) => {
      const tr = [...document.querySelectorAll('.people tbody tr')].find(
        (r) => r.querySelector('strong')?.textContent.trim() === n,
      );
      if (!tr) return null;
      return {
        owner: !!tr.querySelector('.tag'),
        link: !!tr.querySelector('button'),
        rooms: [...tr.querySelectorAll('input[type=checkbox]')].map((c) => ({
          what: c.getAttribute('aria-label'),
          on: c.checked,
          fixed: c.disabled,
        })),
      };
    }, who);

  is('everybody invited is on it',
    await boss.page.evaluate(() =>
      [...document.querySelectorAll('.people tbody tr strong')].map((e) => e.textContent.trim())),
    ['Alex', 'Ryan']);
  is('the owner is marked, and the house is ticked and not hers to untick',
    await row(boss.page, 'Ryan'),
    { owner: true, link: true, rooms: [
      { what: 'Ryan may go to Studio', on: true, fixed: true },
      { what: 'Ryan may go to Cellar', on: true, fixed: true },
    ] });
  is('and Alex has been given nothing',
    (await row(boss.page, 'Alex')).rooms.map((r) => r.on), [false, false]);
  is('the lobby is nobody’s to give, so it is not a column',
    await boss.page.evaluate(() =>
      [...document.querySelectorAll('.people thead th.room')].map((e) => e.textContent.trim())),
    ['Studio', 'Cellar']);

  // Give Alex the Studio: it appears on her list where she sits.
  await boss.page.click('.people input[aria-label="Alex may go to Studio"]');
  await until(() => listed(alex).includes('Studio'));
  is('given a room, she is shown it at once', listed(alex), ['@Alex', 'Lobby', 'Studio']);
  alex.send({ type: 'enter_room', id: 'Studio' });
  await wait(600);
  is('and can go into it', alex.where.id, 'Studio');

  // Take it away again: she is shown it no longer. (She stays where she
  // is until she moves -- the room does not throw anybody out
  // mid-sentence.)
  await boss.page.click('.people input[aria-label="Alex may go to Studio"]');
  await until(() => !listed(alex).includes('Studio'));
  is('taken away, it is off her list', listed(alex), ['@Alex', 'Lobby']);
  alex.send({ type: 'enter_room', id: 'Lobby' });
  await wait(500);
  alex.send({ type: 'enter_room', id: 'Studio' });
  await wait(600);
  is('and shut to her again', alex.where.id, 'Lobby');

  // Invite somebody from the page, and the link it hands back works.
  await boss.page.fill('.people .invite input', 'Mo');
  await boss.page.click('.people .invite button');
  await boss.page.waitForSelector('.people .link code');
  const link = (await boss.page.textContent('.people .link code')).trim();
  is('the link is one of this room’s', link.startsWith(BASE + '/join/'), true);
  const mo = await person(link);
  is('and it lets them in', await mo.page.evaluate(() => !!document.querySelector('.log')), true);
  is('with the lobby and a room of their own',
    await mo.page.evaluate(() =>
      [...document.querySelectorAll('.rooms .room > button .name')].map((e) => e.textContent.trim())),
    ['You and Vox', 'Lobby']);
  await mo.browser.close();

  // None of it is anybody else's: not the page, and not the API behind
  // it. Asked with Alex's own cookie, the room says there is no such
  // thing -- which is all it says.
  const hers = alex.cookie;
  is('a plain user is told the people are not there',
    (await asked(hers, '/api/people')).status, 404);
  is('and cannot invite anybody',
    (await asked(hers, '/api/people', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ name: 'Trouble' }),
    })).status, 404);
  is('nor give herself a room',
    (await asked(hers, '/api/people/Alex/rooms', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ room: 'Cellar', allowed: true }),
    })).status, 404);
  await wait(400);
  is('and after all that she still has what she had', listed(alex), ['@Alex', 'Lobby']);

  // The owner cannot remove themselves from the browser: a house with
  // nobody over it is not something to do by clicking a row.
  is('the owner has no Remove of their own',
    await boss.page.evaluate(() => {
      const tr = [...document.querySelectorAll('.people tbody tr')].find(
        (r) => r.querySelector('strong')?.textContent.trim() === 'Ryan',
      );
      return !!tr.querySelector('button.danger');
    }), false);

  await boss.browser.close();
  report();
})();
