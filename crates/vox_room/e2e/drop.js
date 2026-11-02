// A picture dragged into the room:
//   node drop.js RYAN_LINK ALEX_LINK
//
// The whole way through, in a real browser: a file is let go of over
// the room, the tab re-encodes it, the server keeps it, and the card
// comes back down the socket -- to the tab that dropped it and to
// somebody else's tab, because a picture in the conversation is the
// room's and not one screen's.
//
// The drag is synthesised rather than driven by the mouse: Playwright
// cannot pick a file up off the desktop, and a `DataTransfer` built in
// the page carries one exactly as the browser's own drag does.

const { is, report, person, wait } = require('./room.js');
const [ryanLink, alexLink] = process.argv.slice(2);

// An 8x8 PNG: a yellow square on magenta, the same one run.sh gives
// the avatars suite. As hex, because that is the one form that
// survives being typed into two languages. Smaller than the square
// the card draws it in, which is one half of how a picture is drawn;
// `square: 256` below is the other.
const PNG =
  '89504e470d0a1a0a0000000d49484452000000080000000808020000004b6d29dc0000001b' +
  '4944415478da6338a1b1002b622020f1ff8e061cd14a8204570100f6df6b312bc415490000' +
  '000049454e44ae426082';

/// Hand the room a file, the way somebody does: on a drag let go of
/// over it, or on the clipboard with Ctrl+V. All of it happens in the
/// page, because a `DataTransfer` cannot be carried across.
///
/// The file is `hex`, or drawn `square` pixels across when what is
/// wanted is a picture bigger than the square that shows it -- which
/// is what a photograph is here, and not a thing to carry about as
/// hex. With neither it is not a picture at all.
///
/// `enter` leaves the drag hanging over the room rather than letting
/// go, which is when the room says what a drop would do. `text` puts
/// a line of text on the clipboard beside the file, which is what
/// copying part of a web page does -- and that is a paste meant for
/// whatever has the cursor, not for the room.
const handOver = (page, how, opts) =>
  page.evaluate(
    async ({ how, name, type, hex, square, text, enter }) => {
      let bytes;
      if (square) {
        const c = document.createElement('canvas');
        c.width = c.height = square;
        const x = c.getContext('2d');
        const g = x.createLinearGradient(0, 0, c.width, c.height);
        g.addColorStop(0, '#c0f');
        g.addColorStop(1, '#fd0');
        x.fillStyle = g;
        x.fillRect(0, 0, c.width, c.height);
        bytes = await new Promise((done) => c.toBlob(done, 'image/png'));
      } else if (hex) {
        bytes = new Uint8Array(hex.match(/../g).map((h) => parseInt(h, 16)));
      } else {
        bytes = new Uint8Array([110, 111, 112, 101]);
      }
      const dt = new DataTransfer();
      dt.items.add(new File([bytes], name, { type }));
      if (text) dt.setData('text/plain', text);
      if (how === 'paste') {
        window.dispatchEvent(
          new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }),
        );
        return;
      }
      const app = document.querySelector('.app');
      const fire = (kind) =>
        app.dispatchEvent(
          new DragEvent(kind, { dataTransfer: dt, bubbles: true, cancelable: true }),
        );
      fire('dragenter');
      fire('dragover');
      if (!enter) fire('drop');
    },
    { how, ...opts },
  );

const dropOn = (page, opts) => handOver(page, 'drag', opts);
const pasteOn = (page, opts) => handOver(page, 'paste', opts);

/// The picture cards on this screen, as a person reads them.
const cardsOn = (page) =>
  page.evaluate(() =>
    [...document.querySelectorAll('.entry.picture-entry')].map((e) => {
      const img = e.querySelector('.shot img');
      const label = e.querySelector('.time')?.textContent.trim() ?? '';
      return {
        who: e.querySelector('.name')?.textContent.trim() ?? '',
        // The time and then what it is called; the number is the half
        // that gets said out loud, and the half worth asserting.
        number: label.split('·').pop().trim(),
        timed: /^\d/.test(label),
        asked: e.querySelector('.asked')?.textContent.trim() ?? '',
        // Whether the browser decoded what came back, rather than
        // firing onerror: 0 means the address answered with nothing.
        drawn: img ? img.naturalWidth : 0,
        spinning: !!e.querySelector('.shot .spinner'),
        // How it is drawn: this one is 8 pixels in a 192-pixel
        // square, so it is being blown up and must not be smoothed.
        drawn_as: img ? getComputedStyle(img).imageRendering : null,
        // The AI's own cards are its colour; one somebody brought in
        // is their line, with their own face on it.
        agent: e.classList.contains('agent'),
        face: !!e.querySelector(':scope > .face'),
      };
    }),
  );

/// Wait until this screen has `n` of them, however long the upload and
/// the socket take between them.
const waitForCards = (page, n) =>
  page.waitForFunction((n) => document.querySelectorAll('.entry.picture-entry').length >= n, n);

(async () => {
  const ryan = await person(ryanLink);
  const alex = await person(alexLink);
  await ryan.page.waitForSelector('.app');

  // Hanging over the room, the room says what letting go would do.
  await dropOn(ryan.page, {
    name: 'me-at-the-lake.png',
    type: 'image/png',
    hex: PNG,
    enter: true,
  });
  await wait(200);
  is(
    'the room says what a drop would do',
    await ryan.page.evaluate(
      () => document.querySelector('.dropping')?.textContent.trim() ?? null,
    ),
    'Drop a picture into the conversation',
  );

  // And letting go of it puts it in the conversation.
  await dropOn(ryan.page, { name: 'me-at-the-lake.png', type: 'image/png', hex: PNG });
  await waitForCards(ryan.page, 1);
  // The picture itself is fetched over HTTP once the card is up.
  await wait(500);
  is(
    'the overlay goes with the drop',
    await ryan.page.evaluate(() => !!document.querySelector('.dropping')),
    false,
  );
  is('the picture is in the conversation', await cardsOn(ryan.page), [
    {
      who: 'Ryan',
      // Numbered in the one sequence the AI's pictures are numbered
      // in: that number is what gets said out loud and what
      // set_avatar takes.
      number: 'picture 1',
      timed: true,
      asked: 'me-at-the-lake.png',
      drawn: 8,
      // Nothing was ever waited for: it arrived whole.
      spinning: false,
      // Smaller than the square it is shown in, so it is being blown
      // up -- and a picture being blown up is drawn pixel by pixel.
      // The other way round, which is what a photograph is, is the
      // same question asked of a bigger number.
      drawn_as: 'pixelated',
      agent: false,
      face: true,
    },
  ]);

  await ryan.page.screenshot({ path: '/tmp/vox-room-drop.png' });

  // Everybody's screen, not the one that dropped it: it went into the
  // conversation, which is the room's.
  await waitForCards(alex.page, 1);
  await wait(500);
  is(
    "and on somebody else's screen",
    (await cardsOn(alex.page)).map((c) => ({ who: c.who, number: c.number, drawn: c.drawn })),
    [{ who: 'Ryan', number: 'picture 1', drawn: 8 }],
  );

  // A second one gets the next number, so "the second one" is a thing
  // somebody can say and the AI can act on. This one is pasted rather
  // than dragged -- which is how a screenshot gets anywhere -- and it
  // is bigger than the square it is shown in, which is what a
  // photograph is.
  await pasteOn(ryan.page, { name: 'screenshot.png', type: 'image/png', square: 256 });
  await waitForCards(ryan.page, 2);
  await wait(500);
  const two = await cardsOn(ryan.page);
  is(
    'a pasted picture gets the next number',
    two.map((c) => c.number),
    ['picture 1', 'picture 2'],
  );
  is('and came in under its own name', two[1].asked, 'screenshot.png');
  is('at the size it was', two[1].drawn, 256);
  is(
    'and is smoothed, being made smaller rather than bigger',
    two[1].drawn_as,
    'auto',
  );

  // A paste that is text with pictures in it -- part of a web page --
  // belongs to whatever has the cursor, and the room keeps out of it.
  await pasteOn(ryan.page, {
    name: 'from-a-page.png',
    type: 'image/png',
    square: 256,
    text: 'something copied off a web page',
  });
  await wait(600);
  is('a paste that is also text is left alone', (await cardsOn(ryan.page)).length, 2);

  // Anything that is not a picture is refused where it was dropped,
  // and nothing goes into the conversation.
  const footer = () =>
    ryan.page.evaluate(
      () => document.querySelector('footer .hint')?.textContent.trim() ?? null,
    );
  await dropOn(ryan.page, { name: 'notes.txt', type: 'text/plain' });
  await wait(400);
  is('something that is not a picture is refused', await footer(), 'Only pictures can be dropped into the room.');
  is('and leaves no card behind', (await cardsOn(ryan.page)).length, 2);

  // And said in the words of what was actually done.
  await pasteOn(ryan.page, { name: 'notes.txt', type: 'text/plain' });
  await wait(400);
  is('pasting one says so the same way', await footer(), 'Only pictures can be pasted into the room.');

  // A conversation the room has finished with has nowhere to put a
  // picture -- and a drop the page doesn't take is one the browser
  // opens, which would take the room and the audio with it. So the
  // page takes it and refuses it in words, and is still here after.
  await ryan.page.click('.bar button:has-text("New conversation")');
  await ryan.page.waitForTimeout(300);
  await ryan.page.click('dialog.ask button.primary');
  await ryan.page.waitForTimeout(1400);
  await ryan.page.click('.picker > button');
  await ryan.page.waitForTimeout(800);
  await ryan.page.click('.menu .row:last-child .item');
  await ryan.page.waitForTimeout(900);
  await dropOn(ryan.page, { name: 'me-at-the-lake.png', type: 'image/png', hex: PNG });
  await wait(600);
  is(
    'and an old conversation is nowhere to drop one',
    await ryan.page.evaluate(
      () => document.querySelector('footer .hint')?.textContent.trim() ?? null,
    ),
    'That conversation is over; come back to the one the room is having.',
  );
  is(
    'with the room still on the screen',
    await ryan.page.evaluate(() => !!document.querySelector('.app') && location.pathname),
    '/lobby',
  );
  // The one being read is the one they were dropped into, so this is
  // also the archive still having them: a picture is part of what was
  // said, and is read back with it.
  is('and the two it already had are still all it has', (await cardsOn(ryan.page)).length, 2);

  await ryan.browser.close();
  await alex.browser.close();
  report();
})();
