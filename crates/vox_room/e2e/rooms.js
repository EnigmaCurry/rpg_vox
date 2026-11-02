// A server is several rooms, and moving between them costs nothing:
//   node rooms.js RYAN_LINK ALEX_LINK
//
// No browser at all — two sockets are enough for everything here, which
// is room state rather than anything you have to hear or look at: where
// a tab lands, what each person may go to, that a room of your own
// admits one person, that nothing said in it reaches the house, and that
// the mix keeps arriving across every move.
//
// Wants a room started with two shared rooms:
//   vox_room --room Main --room Studio --echo --no-stt --tts none

const { is, report, wait, socket } = require('./room.js');
const [ryanToken, alexToken] = process.argv.slice(2).map((l) => l.replace(/.*\/join\//, ''));

(async () => {
  const ryan = await socket(ryanToken);
  const alex = await socket(alexToken);
  await wait(400);

  is('a tab lands in the first room named', ryan.where.id, 'Main');
  is('his own first, then the house', ryan.rooms.map((r) => r.id), ['@Ryan', 'Main', 'Studio']);
  is('hers first, then the house', alex.rooms.map((r) => r.id), ['@Alex', 'Main', 'Studio']);
  is('both are in Main', ryan.rooms.find((r) => r.id === 'Main').here, ['Alex', 'Ryan']);
  is('the roster is who is in Main', ryan.people, ['Alex', 'Ryan']);
  is('one language, so nothing to say about who speaks what', ryan.speaks, null);
  is('Main waits to be named', ryan.mode, 'named');

  // Something said in Main, so there is a log to tell the rooms apart by.
  ryan.send({ type: 'mode', bot: 'Vox', mode: 'always' });
  ryan.send({ type: 'text', text: 'this is the shared room' });
  await wait(2500);

  // Ryan steps into his own room.
  ryan.send({ type: 'enter_room', id: '@Ryan' });
  await wait(600);
  is('Ryan is in his own room', ryan.where.id, '@Ryan');
  is('it says it is private', ryan.where.private, true);
  is('it is called after the AI', ryan.where.title, 'You and Vox');
  is('its roster is him alone', ryan.people, ['Ryan']);
  is('its mixer has no strip for anybody else', ryan.strips.includes('Alex'), false);
  is('it answers every pause by default', ryan.mode, 'always');
  is('nothing from Main is in it', ryan.log, []);
  is('Alex no longer has him in Main', alex.rooms.find((r) => r.id === 'Main').here, ['Alex']);
  is('and his room is not on her list', alex.rooms.some((r) => r.id === '@Ryan'), false);

  // The AI answers him there, and Alex hears nothing of it.
  ryan.send({ type: 'text', text: 'just the two of us now' });
  await wait(2500);
  is('the AI answered in his own room', /^Vox: Ryan said just/.test(ryan.last ?? ''), true);
  is('not a word of it reached Alex', /two of us/.test(alex.seen), false);

  // Alex cannot ask for it.
  alex.send({ type: 'enter_room', id: '@Ryan' });
  await wait(400);
  is('Alex is turned away from his room', alex.where.id, 'Main');
  is('and told so', alex.notices, ["That room isn't here."]);

  // Back to Main: the conversation he left is still there.
  ryan.send({ type: 'enter_room', id: 'Main' });
  await wait(600);
  is('Ryan is back in Main', ryan.where.id, 'Main');
  is('with what was said in it', ryan.log.map((l) => l.split(':')[0]), ['Ryan', 'Vox']);
  is('and nothing of his own room', ryan.log.some((l) => /two of us/.test(l)), false);
  is('and Alex has him again', alex.rooms.find((r) => r.id === 'Main').here, ['Alex', 'Ryan']);

  // The other shared room is empty and its own.
  alex.send({ type: 'enter_room', id: 'Studio' });
  await wait(600);
  is('Alex moved to Studio', alex.where.id, 'Studio');
  is('which has nothing in it', alex.log, []);
  is('and she is off the Main roster, not away on it', ryan.people, ['Ryan']);
  is('Ryan sees her in Studio', ryan.rooms.find((r) => r.id === 'Studio').here, ['Alex']);

  // The audio never stopped: frames kept arriving across every move.
  is('the mix kept coming', ryan.frames > 20, true);

  report();
})();
