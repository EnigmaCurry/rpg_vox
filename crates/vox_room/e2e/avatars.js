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
