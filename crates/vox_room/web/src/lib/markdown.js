// What a card is written in.
//
// A card is the room's way of putting something in front of people
// instead of saying it: the standings, a forecast, the rules as agreed.
// Those want a table or a list to be read at a glance, so a card's body
// is markdown — the one place in the room where it is, because it is
// the one place that is written to be looked at rather than heard.
//
// A small subset on purpose: headings, bold, italic, strikethrough,
// code, lists, tables, quotes, rules and links. No images, no
// footnotes, nothing that fetches anything, and **no raw HTML at all**.
//
// Everything is escaped before a single tag is added, so nothing a
// model writes can become markup: every tag in the output was put there
// by this file, and `<script>` in a card is three words on the screen.
// That invariant is the whole of why the result is safe to hand to
// `{@html}`, and anything added here has to keep it — escape first,
// then wrap.

const ESCAPES = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
const escape = (s) => s.replace(/[&<>"']/g, (c) => ESCAPES[c]);

/// A link the browser may follow. Anything else — `javascript:`,
/// `data:`, a scheme nobody has heard of — is not a link, and its text
/// is left as text rather than quietly dropped.
const safeUrl = (url) => /^(https?:\/\/|mailto:)/i.test(url);

const link = (url, label) =>
  `<a href="${url}" target="_blank" rel="noopener noreferrer">${label}</a>`;

/// The inline run inside one line.
///
/// Code and links are built first and held aside as `<n>`, so that
/// what is in backticks is left exactly as written and an underscore in
/// a URL is an underscore. The emphasis passes then run over text that
/// has no tags in it at all, and the held pieces go back last.
function inline(text) {
  const held = [];
  // Nothing in the text can be mistaken for one of these: it has been
  // escaped, so the only `<` left in it are the ones written here.
  const hold = (html) => `<${held.push(html) - 1}>`;
  let out = text
    .replace(/`([^`]+)`/g, (_, code) => hold(`<code>${code}</code>`))
    .replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (whole, label, url) =>
      safeUrl(url) ? hold(link(url, label)) : whole,
    )
    // A bare address is a link too: a card is read, and a forecast says
    // where it came from.
    .replace(
      /(^|[\s(])(https?:\/\/[^\s<]+[^\s<.,:;)\]])/g,
      (_, before, url) => `${before}${hold(link(url, url))}`,
    )
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/__([^_]+)__/g, '<strong>$1</strong>')
    .replace(/(^|[^*\w])\*([^*\n]+)\*/g, '$1<em>$2</em>')
    .replace(/(^|[^_\w])_([^_\n]+)_/g, '$1<em>$2</em>')
    .replace(/~~([^~]+)~~/g, '<del>$1</del>');
  return out.replace(/<(\d+)>/g, (_, n) => held[Number(n)]);
}

/// The cells of one table row, without the edges it may or may not have
/// been written with.
const cells = (line) =>
  line
    .trim()
    .replace(/^\||\|$/g, '')
    .split('|')
    .map((c) => c.trim());

/// Whether this is the `|---|:--:|` line that makes the row above it a
/// heading — and the thing that tells a table from a line with pipes in
/// it.
const ruled = (line) => /^\s*\|?[\s:|-]*-[\s:|-]*\|?\s*$/.test(line) && line.includes('-');

/// Where a cell's text sits, from the colons in the rule under it.
function alignment(spec) {
  const left = spec.startsWith(':');
  const right = spec.endsWith(':');
  if (left && right) return ' style="text-align:center"';
  if (right) return ' style="text-align:right"';
  return '';
}

const listItem = (line) => /^(\s*)([-*+]|\d+[.)])\s+(.*)$/.exec(line);

/// A quoted line, as it looks once it has been escaped.
const QUOTE = /^\s*&gt;/;

/// The blocks of already-escaped lines. Recursion comes back here and
/// not to `markdown`, or the escaping would be done twice and an
/// ampersand inside a quote would arrive as `&amp;amp;`.
function blocks(lines) {
  const out = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) {
      i++;
      continue;
    }

    // ``` fenced code: everything to the closing fence, exactly as
    // written. An unclosed fence runs to the end, which is what
    // opening it said to do.
    if (/^\s*```+\s*[\w+-]*\s*$/.test(line)) {
      const body = [];
      i++;
      while (i < lines.length && !/^\s*```+\s*$/.test(lines[i])) body.push(lines[i++]);
      i++;
      out.push(`<pre><code>${body.join('\n')}</code></pre>`);
      continue;
    }

    // # a heading. The card has a title of its own above all this, so
    // these start small and stay there.
    const head = /^\s*(#{1,6})\s+(.*)$/.exec(line);
    if (head) {
      const level = Math.min(6, 3 + head[1].length);
      out.push(`<h${level}>${inline(head[2].trim())}</h${level}>`);
      i++;
      continue;
    }

    // --- a rule.
    if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) {
      out.push('<hr>');
      i++;
      continue;
    }

    // | a | table |
    if (line.includes('|') && i + 1 < lines.length && ruled(lines[i + 1])) {
      const heads = cells(line);
      const align = cells(lines[i + 1]).map(alignment);
      i += 2;
      const rows = [];
      while (i < lines.length && lines[i].trim() && lines[i].includes('|')) {
        rows.push(cells(lines[i++]));
      }
      const th = heads.map((c, n) => `<th${align[n] ?? ''}>${inline(c)}</th>`).join('');
      const tb = rows
        .map((r) => `<tr>${r.map((c, n) => `<td${align[n] ?? ''}>${inline(c)}</td>`).join('')}</tr>`)
        .join('');
      out.push(`<table><thead><tr>${th}</tr></thead><tbody>${tb}</tbody></table>`);
      continue;
    }

    // > a quote. These lines have been escaped, so the mark to look
    // for is what escaping made of it.
    if (QUOTE.test(line)) {
      const body = [];
      while (i < lines.length && QUOTE.test(lines[i])) {
        body.push(lines[i++].replace(/^\s*&gt;\s?/, ''));
      }
      out.push(`<blockquote>${blocks(body)}</blockquote>`);
      continue;
    }

    // - a list, or 1. a numbered one. Two spaces of indent is a list
    // inside a list, and that is as deep as a card goes.
    const first = listItem(line);
    if (first) {
      const ordered = /\d/.test(first[2]);
      const items = [];
      while (i < lines.length) {
        const m = listItem(lines[i]);
        if (!m || /\d/.test(m[2]) !== ordered) break;
        if (m[1].length >= 2 && items.length) {
          items[items.length - 1].under.push(lines[i].replace(/^\s{2}/, ''));
        } else {
          items.push({ text: m[3], under: [] });
        }
        i++;
      }
      const li = items
        .map((it) => `<li>${inline(it.text)}${it.under.length ? blocks(it.under) : ''}</li>`)
        .join('');
      out.push(ordered ? `<ol>${li}</ol>` : `<ul>${li}</ul>`);
      continue;
    }

    // Anything else is a paragraph, and the line breaks in it are kept:
    // the room keeps the shape of what was written everywhere else, and
    // a card is where it matters most.
    const para = [];
    while (
      i < lines.length &&
      lines[i].trim() &&
      !listItem(lines[i]) &&
      !/^\s*(&gt;|#{1,6}\s|```)/.test(lines[i])
    ) {
      para.push(lines[i++]);
    }
    out.push(`<p>${para.map((l) => inline(l.trim())).join('<br>')}</p>`);
  }
  return out.join('');
}

/// Markdown to HTML, for a card's body. The input is whatever was
/// written; the output is safe to render.
export function markdown(src) {
  const text = String(src ?? '')
    .replace(/\r\n?/g, '\n');
  return blocks(escape(text).split('\n'));
}
