// What a card looks like once it is on the screen:
//   node cards.js RYAN_LINK
//
// A card is the one thing in the room written to be read rather than
// heard, so its body is markdown. This opens a room with one already in
// the conversation -- run.sh puts it there, since posting one is a thing
// only a model does -- and reads the markup back out of the page.
//
// The last of these is the one that matters: a card is written by a
// model, and a model writes whatever it was told to write. Nothing in a
// card may become markup, so the tags in it arrive as words.

const { is, report, person, wait } = require('./room.js');
const [ryanLink] = process.argv.slice(2);

(async () => {
  const ryan = await person(ryanLink);
  await ryan.page.waitForSelector('.card.posted');
  await wait(300);

  const card = (what) => ryan.page.evaluate((s) => {
    const body = document.querySelector('.card.posted .body');
    const pick = (q) => [...body.querySelectorAll(q)].map((e) => e.textContent.trim());
    return {
      title: document.querySelector('.card.posted .title').textContent.trim(),
      heads: pick('table thead th'),
      cells: pick('table tbody td'),
      items: pick('ul li'),
      bold: pick('strong'),
      code: pick('pre code'),
      links: [...body.querySelectorAll('a')].map((a) => [a.textContent.trim(), a.getAttribute('href')]),
      // Everything the card says, as a person reads it.
      text: body.textContent.replace(/\s+/g, ' ').trim(),
      // And every element it became, with whether any of them carries
      // a handler. The words "script" and "onerror" are allowed to be
      // on the screen; what must not exist is the thing itself.
      tags: [...new Set([...body.querySelectorAll('*')].map((e) => e.tagName.toLowerCase()))].sort(),
      handlers: [...body.querySelectorAll('*')].some((e) =>
        [...e.attributes].some((a) => a.name.startsWith('on')),
      ),
    };
  }, what);

  const c = await card();
  is('the card has its title', c.title, 'Boulder, next three days');
  is('a table is a table', c.heads, ['Day', 'High', 'Low']);
  is('with its figures in it', c.cells, ['Thu', '14°C', '2°C', 'Fri', '9°C', '-1°C']);
  is('a list is a list', c.items, ['Wind picks up Friday afternoon', 'Bring a coat after 6pm']);
  is('bold is bold', c.bold, ['Boulder, Colorado']);
  is('and a code block keeps what is in it', c.code, ['wind = 40 km/h']);
  is(
    'a link is one, and opens away from the room',
    c.links,
    [['the weather office', 'https://weather.gov/boulder']],
  );

  // What a card must never do. The body had a script tag and an
  // onerror image in it; neither became anything.
  is(
    'nothing in a card becomes a tag of its own',
    c.tags.filter(
      (t) =>
        // `div` is the box a table scrolls inside, and the only one
        // the renderer makes.
        !['a', 'br', 'code', 'del', 'div', 'em', 'li', 'p', 'pre', 'strong', 'table', 'tbody', 'td', 'th', 'thead', 'tr', 'ul'].includes(t),
    ),
    [],
  );
  is('and nothing carries a handler', c.handlers, false);
  is(
    'a script tag in a card is words on the screen',
    c.text.includes('<script>alert(1)</script>'),
    true,
  );
  is(
    'a javascript: link is not a link',
    c.links.some(([, href]) => (href || '').startsWith('javascript:')),
    false,
  );

  await ryan.browser.close();
  report();
})();
