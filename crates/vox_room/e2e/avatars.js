// The square beside everything somebody says:
//   node avatars.js RYAN_LINK
//
// Every line in the conversation carries a face: the picture when one
// has been made, and otherwise the tint that name already has with its
// initials in it. The column has to line up either way, which is the
// thing worth checking on a real screen -- a placeholder of a
// different size would unalign every name under it.
//
// run.sh puts the pictures and the lines there, since making one is a
// thing only a model with a picture server does.

const { is, report, person, wait } = require('./room.js');
const [ryanLink] = process.argv.slice(2);

(async () => {
  const ryan = await person(ryanLink);
  await ryan.page.waitForSelector('.entry .face');
  // The pictures are fetched over HTTP, so give them a moment to land
  // before asking which branch each square took.
  await wait(500);

  const faces = await ryan.page.evaluate(() =>
    [...document.querySelectorAll('.said > .entry')].map((e) => {
      const face = e.querySelector(':scope > .face');
      const img = face?.querySelector('img');
      const box = face?.getBoundingClientRect();
      return {
        who: e.querySelector('.name')?.textContent.trim() ?? '',
        // Which branch drew it: a picture, or their initials.
        src: img?.getAttribute('src') ?? null,
        // Whether the browser actually decoded it, rather than firing
        // onerror and falling back. 0 means it never loaded.
        drawn: img ? img.naturalWidth : 0,
        initials: face?.querySelector('.initials')?.textContent.trim() ?? null,
        w: box ? Math.round(box.width) : 0,
        h: box ? Math.round(box.height) : 0,
      };
    }),
  );

  is('every line has a square', faces.length >= 3 && faces.every((f) => f.w > 0), true);
  is(
    'and every square is the same square',
    new Set(faces.map((f) => `${f.w}x${f.h}`)).size,
    1,
  );
  is('which is square', faces[0].w, faces[0].h);

  await ryan.page.screenshot({ path: '/tmp/vox-room-avatars.png' });

  const ryansOwn = faces.find((f) => f.who === 'Ryan');
  const voxs = faces.find((f) => f.who === 'Vox');
  const alexs = faces.find((f) => f.who === 'Alex');

  // A face that was made: the picture, at the version the room sent,
  // and actually decoded by the browser.
  is('Ryan has a picture', /^\/api\/avatar\/Ryan\?v=\d+$/.test(ryansOwn.src ?? ''), true);
  is('and the browser drew it', ryansOwn.drawn, 8);
  is('so no initials in its place', ryansOwn.initials, null);

  // The AI has one too, under its own name.
  is('Vox has a picture', /^\/api\/avatar\/Vox\?v=\d+$/.test(voxs.src ?? ''), true);
  is('and the browser drew that', voxs.drawn, 8);

  // And nobody who has none is left with a hole: the tint of their
  // name, with enough of it to tell two people apart.
  is('Alex has no picture', alexs.src, null);
  is('and shows initials instead', alexs.initials, 'A');

  // Asked for once per version, however many lines that person said:
  // the address carries the version and the answer is immutable.
  const fetched = await ryan.page.evaluate(() =>
    performance
      .getEntriesByType('resource')
      .filter((r) => r.name.includes('/api/avatar/'))
      .map((r) => r.name.replace(/^https?:\/\/[^/]+/, '')),
  );
  is(
    "Ryan's picture was fetched once",
    fetched.filter((u) => u.startsWith('/api/avatar/Ryan')).length,
    1,
  );

  // A picture the AI made, in the conversation where the room can
  // look at it and say what to change.
  const cards = await ryan.page.evaluate(() =>
    [...document.querySelectorAll('.entry.picture-entry')].map((e) => {
      const img = e.querySelector('.shot img');
      return {
        label: e.querySelector('.time')?.textContent.trim() ?? '',
        asked: e.querySelector('.asked')?.textContent.trim() ?? '',
        src: img?.getAttribute('src') ?? null,
        drawn: img ? img.naturalWidth : 0,
        spinning: !!e.querySelector('.shot .spinner'),
        broke: e.querySelector('.shot .broke')?.textContent.trim() ?? null,
        why: e.querySelector('.hint')?.textContent.trim() ?? null,
      };
    }),
  );
  is('both pictures are in the conversation', cards.length, 2);
  // Numbered, because the number is what somebody says out loud and
  // what set_avatar is given.
  is('the first is numbered', cards[0].label.endsWith('picture 1'), true);
  is('and shows what it was asked for', cards[0].asked, 'a tired wizard');
  is('and the browser drew it', cards[0].drawn, 8);
  is('with no spinner on it', cards[0].spinning, false);

  // The second was still being drawn when the room was last stopped,
  // which is the one case a spinner must not survive: the call it was
  // waiting on died with the process, so the card says so rather than
  // promising for ever that something is still happening. (The
  // spinner itself is a live state, checked in the Rust tests where a
  // picture can be begun without finishing it.)
  is('the second is numbered too', cards[1].label.endsWith('picture 2'), true);
  is('and says it never came', cards[1].broke, "didn't come");
  is('rather than spinning for ever', cards[1].spinning, false);
  is('with the reason under it', cards[1].why, 'the room restarted while it was being made');
  is('and no image', cards[1].src, null);
  is('but it still says what it would have been', cards[1].asked, 'the same wizard, rested');

  // Clicking a face opens who they are.
  await ryan.page.evaluate(() => {
    const mine = [...document.querySelectorAll('.said > .entry')].find(
      (e) => e.querySelector('.name')?.textContent.trim() === 'Ryan',
    );
    mine.querySelector(':scope > .face').click();
  });
  await ryan.page.waitForSelector('.who-card');
  await wait(300);
  is(
    'a face opens onto who they are',
    await ryan.page.evaluate(() => {
      const c = document.querySelector('.who-card');
      return {
        who: c.querySelector('h2').textContent.trim(),
        bio: c.querySelector('.bio')?.textContent.trim() ?? null,
        asked: c.querySelector('.asked')?.textContent.replace(/\s+/g, ' ').trim() ?? null,
        tags: [...c.querySelectorAll('.tag')].map((t) => t.textContent.trim()),
        drawn: c.querySelector('img')?.naturalWidth ?? 0,
      };
    }),
    {
      who: 'Ryan',
      bio: 'A ranger from the north, quiet about it.',
      asked: 'drawn as a tired wizard',
      tags: ['reads English', 'here'],
      drawn: 8,
    },
  );

  // Escape closes it, and nobody else's screen ever had it.
  await ryan.page.keyboard.press('Escape');
  await wait(200);
  is(
    'and closes again',
    await ryan.page.evaluate(() => !!document.querySelector('.who-card')),
    false,
  );

  // An AI has one too, and the card says it is not a person.
  await ryan.page.evaluate(() => {
    const vox = [...document.querySelectorAll('.said > .entry')].find(
      (e) => e.querySelector('.name')?.textContent.trim() === 'Vox',
    );
    vox.querySelector(':scope > .face').click();
  });
  await ryan.page.waitForSelector('.who-card');
  await wait(300);
  is(
    'an AI is not a person, and says so',
    await ryan.page.evaluate(() => {
      const c = document.querySelector('.who-card');
      return {
        who: c.querySelector('h2').textContent.trim(),
        tags: [...c.querySelectorAll('.tag')].map((t) => t.textContent.trim()),
      };
    }),
    { who: 'Vox', tags: ['an AI', 'speaks English', 'here'] },
  );
  await ryan.page.screenshot({ path: '/tmp/vox-room-profile.png' });
  await ryan.page.keyboard.press('Escape');
  await wait(200);

  // Yours to take off, from the one place that offers it. The room
  // tells every tab the moment it goes, so the square in the
  // conversation behind this page turns back into initials without a
  // reload -- which is the half a DELETE on its own wouldn't prove.
  await ryan.page.click('.tools button.cog');
  await ryan.page.waitForSelector('.settings');
  is(
    'Settings offers to take your face off',
    await ryan.page.evaluate(() =>
      [...document.querySelectorAll('.settings button')].some(
        (b) => b.textContent.trim() === 'Take it off',
      ),
    ),
    true,
  );
  await ryan.page.evaluate(() =>
    [...document.querySelectorAll('.settings button')]
      .find((b) => b.textContent.trim() === 'Take it off')
      .click(),
  );
  await wait(500);
  is(
    'and it is gone, with the initials back in its place',
    await ryan.page.evaluate(() => {
      const face = document.querySelector('.settings .face-row .face');
      return { img: !!face?.querySelector('img'), initials: face?.querySelector('.initials')?.textContent.trim() };
    }),
    { img: false, initials: 'R' },
  );

  // And in the conversation, which nobody reloaded.
  await ryan.page.click('.settings header button.primary');
  await ryan.page.waitForSelector('.said');
  is(
    'including beside what they said',
    await ryan.page.evaluate(() => {
      const mine = [...document.querySelectorAll('.said > .entry')].find(
        (e) => e.querySelector('.name')?.textContent.trim() === 'Ryan',
      );
      return !!mine.querySelector(':scope > .face img');
    }),
    false,
  );

  await ryan.browser.close();
  report();
})();
