<script>
  // Split an assistant turn's raw text at every `<speak>…</speak>` block,
  // rendering the text chunks as markdown and each speech block as an
  // inline SpeechInline widget. We split BEFORE markdown parsing so the
  // angle brackets in `<speak>` never get mangled by the markdown parser
  // (unknown HTML tags survive marked's default settings, but doing our
  // own split keeps the code robust to future marked configuration and
  // gives us a clean seam to slot Svelte components into).
  //
  // Server has already parsed the same regex and produced one persistent
  // block row per `<speak>` — the `blocks` prop is that ordered list.
  // A count/order mismatch between the raw text and the blocks list means
  // the text was edited after render (not currently possible), so we bail
  // to marked-only rendering rather than pair the wrong block to the wrong
  // slot.

  import DOMPurify from 'dompurify';
  import { marked } from 'marked';
  import SpeechInline from './SpeechInline.svelte';

  let {
    content = '',
    blocks = [],
    onblockchange = null,
    /// Forwarded to every inline SpeechInline: when true, blocks with no
    /// takes auto-render their first take on mount. Parent sets this only
    /// for the most-recent assistant turn so a page reload doesn't
    /// re-trigger TTS for every past speech block.
    autoRender = false,
  } = $props();

  const SPEAK_TAG = /<speak>([\s\S]*?)<\/speak>/gi;

  // marked's default is compatible with common LLM output; disable smart-lists
  // etc so ordinary chat bullets don't misrender. Sanitizer runs after.
  marked.setOptions({ gfm: true, breaks: true });

  function renderMarkdown(md) {
    if (!md || !md.trim()) return '';
    // marked can return a promise depending on options; passing a string with
    // async: false (default) keeps it synchronous.
    const raw = marked.parse(md);
    return DOMPurify.sanitize(raw, { USE_PROFILES: { html: true } });
  }

  /// Walk the raw content and produce an ordered `parts` array of alternating
  /// { kind: 'md', html } and { kind: 'speak', block } entries. When the
  /// number of `<speak>` matches doesn't line up with `blocks.length`, fall
  /// back to a single { kind: 'md' } entry with the whole content.
  function buildParts(content, blocks) {
    if (!content) return [];
    const matches = Array.from(content.matchAll(SPEAK_TAG));
    if (matches.length !== blocks.length) {
      // Best-effort recovery: if the persisted block count doesn't match
      // the regex-found count in the (possibly edited) content, render
      // everything as plain markdown rather than pair the wrong block to
      // the wrong slot.
      return [{ kind: 'md', html: renderMarkdown(content) }];
    }
    const parts = [];
    let cursor = 0;
    matches.forEach((m, i) => {
      const leading = content.slice(cursor, m.index);
      if (leading) parts.push({ kind: 'md', html: renderMarkdown(leading) });
      parts.push({ kind: 'speak', block: blocks[i] });
      cursor = m.index + m[0].length;
    });
    const trailing = content.slice(cursor);
    if (trailing) parts.push({ kind: 'md', html: renderMarkdown(trailing) });
    return parts;
  }

  const parts = $derived(buildParts(content, blocks));
</script>

{#each parts as p, i (i)}
  {#if p.kind === 'md'}
    <span class="md">{@html p.html}</span>
  {:else}
    <SpeechInline
      block={p.block}
      {onblockchange}
      {autoRender}
    />
  {/if}
{/each}

<style>
  /* Neutralize marked's block-level margins so an inline mix of markdown +
     speech pills reads as a continuous paragraph. Consumers of this
     component (assistant bubbles) supply their own padding/line-height. */
  .md :global(p) { margin: 0.15em 0; display: inline; }
  .md :global(p + p) { margin-top: 0.4em; display: block; }
  .md :global(ul), .md :global(ol) { margin: 0.3em 0 0.3em 1.2em; padding: 0; }
  .md :global(pre) {
    background: rgba(0, 0, 0, 0.35);
    padding: 8px 10px;
    border-radius: 6px;
    overflow-x: auto;
    font-size: 12px;
    margin: 0.4em 0;
  }
  .md :global(code) {
    background: rgba(0, 0, 0, 0.35);
    padding: 1px 4px;
    border-radius: 4px;
    font-size: 12px;
  }
  .md :global(a) { color: var(--accent); }
  .md :global(strong) { color: var(--text); }
  .md :global(em) { color: var(--text); }
</style>
