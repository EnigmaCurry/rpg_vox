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

const { BASE, is, report, person, socket, until, wait, openRooms, closeRooms } = require('./room.js');
const [ownerLink, alexLink] = process.argv.slice(2);
const alexToken = alexLink.replace(/.*\/join\//, '');

/// The rooms a socket is shown, by id.
const listed = (p) => p.rooms.map((r) => r.id);

/// The id of the room made during this run, whatever it is called by
/// the time we ask: a room made in the room has an id of its own, and
/// that is the whole point of it.
const attic = (p) => (p.rooms.find((r) => !['@Alex', 'Lobby', 'Studio', 'Cellar'].includes(r.id)) ?? {}).id;

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
  await openRooms(boss.page);
  is('the owner has the house',
    await boss.page.evaluate(() =>
      [...document.querySelectorAll('.pick .list button .name')].map((e) => e.textContent.trim())),
    ['You and Vox', 'Lobby', 'Studio', 'Cellar']);
  await closeRooms(boss.page);

  // Only the owner is offered the page at all.
  await boss.page.click('.tools button.cog');
  await boss.page.waitForSelector('.settings');
  is('and is offered the people', await boss.page.evaluate(() => !!document.querySelector('.settings button.manage')), true);
  await boss.page.click('.settings button.manage');
  // The page asks the room before it has anything to show, so wait for
  // what came back rather than for the page.
  await boss.page.waitForSelector('.people tbody tr');

  const row = (page, who) =>
    page.evaluate((n) => {
      const tr = [...document.querySelectorAll('.people table.who tbody tr')].find(
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
      [...document.querySelectorAll('.people table.who tbody tr strong')].map((e) => e.textContent.trim())),
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
      [...document.querySelectorAll('.people table.who thead th.room')].map((e) => e.textContent.trim())),
    ['Studio', 'Cellar']);

  // The AIs are given rooms the same way, with a column for the lobby
  // and one for every room of somebody's own, because a room of your
  // own is yours.
  is('the AI columns are every room, and the private ones at once',
    await boss.page.evaluate(() =>
      [...document.querySelectorAll('.people table.voices thead th.room')].map((e) => e.textContent.trim())),
    ['Lobby', 'Studio', 'Cellar', 'Rooms of their own']);
  // The lobby is the one room with no AI in it: nothing said there is
  // written down, so there is nothing for one to hear. Its tick is
  // empty and fixed, where every other one of the first AI's is on and
  // fixed -- being in a room is what the room's own AI is.
  is("the room's own AI is in every room but the lobby, and none of it is the owner's to untick",
    await boss.page.evaluate(() =>
      [...document.querySelectorAll('.people table.voices tbody input[type=checkbox]')].map((c) => [
        c.getAttribute('aria-label'),
        c.checked,
        c.disabled,
      ])),
    [
      ['Vox is in the lobby', false, true],
      ['Vox is in Studio', true, true],
      ['Vox is in Cellar', true, true],
      ["Vox is in a room of somebody's own", true, true],
    ]);

  // Give Alex the Studio: it appears on her list where she sits.
  await boss.page.click('.people input[aria-label="Alex may go to Studio"]');
  await until(() => listed(alex).includes('Studio'));
  is('given a room, she is shown it at once', listed(alex), ['@Alex', 'Lobby', 'Studio']);
  alex.send({ type: 'enter_room', id: 'Studio' });
  await wait(600);
  is('and can go into it', alex.where.id, 'Studio');

  // Take it away again, while she is standing in it: it goes off her
  // list and she goes with it. A room somebody may not enter is not a
  // room to leave them sitting in -- it would be off every list, with
  // no way out of it but the door.
  await boss.page.click('.people input[aria-label="Alex may go to Studio"]');
  await until(() => alex.where.id === 'Lobby');
  is('taken away, it is off her list', listed(alex), ['@Alex', 'Lobby']);
  is('and she is out of it', alex.where.id, 'Lobby');
  is('and told why', alex.notices.at(-1), "That room isn't yours any more. You are back in the lobby.");
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
  await openRooms(mo.page);
  is('with the lobby and a room of their own',
    await mo.page.evaluate(() =>
      [...document.querySelectorAll('.pick .list button .name')].map((e) => e.textContent.trim())),
    ['You and Vox', 'Lobby']);
  await closeRooms(mo.page);
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

  // -- the house itself, which the owner makes --------------------------

  // A room made in the room. It is the owner's alone until it is given.
  await boss.page.fill('.people .rooms + .note ~ * input, .people section:nth-of-type(2) .invite input', 'The Attic');
  await boss.page.click('.people section:nth-of-type(2) .invite button');
  await boss.page.waitForSelector('.people .rooms li:nth-child(4)');
  is('the house has a new room',
    await boss.page.evaluate(() =>
      [...document.querySelectorAll('.people .rooms li strong')].map((e) => e.textContent.trim())),
    ['Lobby', 'Studio', 'Cellar', 'The Attic']);
  await wait(600);
  is('and nobody else has it yet', listed(alex), ['@Alex', 'Lobby']);

  // Renaming it moves the word and nothing else: Alex is given it,
  // goes in, and the room she is standing in is renamed under her.
  await boss.page.click('.people input[aria-label="Alex may go to The Attic"]');
  await until(() => listed(alex).includes(attic(alex)));
  const atticId = attic(alex);
  alex.send({ type: 'enter_room', id: atticId });
  await wait(700);
  is('she is in it', alex.where.id, atticId);

  await boss.page.click('.people .rooms li:nth-child(4) button');
  await boss.page.fill('.people .rooms li:nth-child(4) input', 'The Loft');
  await boss.page.click('.people .rooms li:nth-child(4) button.primary');
  await until(() => (alex.rooms.find((r) => r.id === atticId) ?? {}).title === 'The Loft');
  is('renamed under her, and it is the same room',
    alex.rooms.find((r) => r.id === atticId).title, 'The Loft');
  is('she is still in it', alex.where.id, atticId);
  is('and it is still hers to go to', listed(alex).includes(atticId), true);

  // Taken away while she is standing in it: she walks back to the
  // lobby by herself.
  await boss.page.click('.people .rooms li:nth-child(4) button.danger');
  await boss.page.waitForSelector('dialog.ask[open]');
  await boss.page.click('dialog.ask button.primary');
  await until(() => alex.where.id === 'Lobby');
  is('a room taken away puts her back in the lobby', alex.where.id, 'Lobby');
  is('and is gone from her list', listed(alex), ['@Alex', 'Lobby']);
  is('and from the house',
    await boss.page.evaluate(() =>
      [...document.querySelectorAll('.people .rooms li strong')].map((e) => e.textContent.trim())),
    ['Lobby', 'Studio', 'Cellar']);

  // The lobby is not the owner's to take away either.
  is('the lobby has no way to be taken away',
    await boss.page.evaluate(() =>
      !!document.querySelector('.people .rooms li:first-child button.danger')), false);

  // The owner cannot remove themselves from the browser: a house with
  // nobody over it is not something to do by clicking a row.
  is('the owner has no Remove of their own',
    await boss.page.evaluate(() => {
      const tr = [...document.querySelectorAll('.people table.who tbody tr')].find(
        (r) => r.querySelector('strong')?.textContent.trim() === 'Ryan',
      );
      return !!tr.querySelector('button.danger');
    }), false);

  await boss.browser.close();
  report();
})();
