// What happens to the room selector's cut corners on hover?
const { person } = require('./room.js');
(async () => {
  const ryan = await person(process.argv[2]);
  const read = async (label) => {
    const v = await ryan.page.evaluate(() => {
      const s = getComputedStyle(document.querySelector('.current.panel'));
      return {
        image: s.backgroundImage.replace(/\s+/g, ' ').slice(0, 120),
        color: s.backgroundColor,
      };
    });
    console.log(label.padEnd(6), v.color, '|', v.image);
  };
  await read('rest');
  await ryan.page.hover('.current.panel');
  await ryan.page.waitForTimeout(250);
  await read('hover');
  await ryan.page.screenshot({ path: '/tmp/vox-room-hover.png' });
  await ryan.browser.close();
})();
