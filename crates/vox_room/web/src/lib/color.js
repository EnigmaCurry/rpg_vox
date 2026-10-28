/// A stable hue for each person, so their lines are easy to tell apart.
export function hue(name) {
  let h = 0;
  for (const c of name || '') h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return h % 360;
}
