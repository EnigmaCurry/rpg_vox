<script>
  // The server now emits ONE block per region — narrator prose AND
  // `<speak>` bodies both become blocks — so this component just walks the
  // ordered blocks list and slots each into a SpeechInline. When the block
  // list is empty (legacy or bug), fall back to marked-rendering the raw
  // content so at least the text is readable.

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
    /// Cursor into the autoplay sequence — the block whose ord matches
    /// this value will auto-play its selected take when ready. Null if
    /// autoplay is off (older turn, user cancelled, or auto-render off).
    autoPlayOrd = null,
    /// Called after a block finishes auto-playing so the parent can
    /// advance `autoPlayOrd` to the next block.
    onAutoAdvance = null,
    /// Called on any manual interaction with any block (play/render/stop/
    /// delete). Parent nulls out `autoPlayOrd` so the queue stops.
    onAutoInterrupt = null,
    /// Agent id whose voice slots pick the character voice for each new
    /// take. Passed through to SpeechInline's createTake calls. Null =
    /// server uses the hardcoded role-based DSP fallback.
    agentId = null,
  } = $props();

  marked.setOptions({ gfm: true, breaks: true });

  function renderMarkdown(md) {
    if (!md || !md.trim()) return '';
    const raw = marked.parse(md);
    return DOMPurify.sanitize(raw, { USE_PROFILES: { html: true } });
  }

  // Fallback when there are no blocks (legacy turns, or the server didn't
  // extract any regions). Renders the raw content as sanitized markdown.
  const fallbackHtml = $derived(renderMarkdown(content));
</script>

{#if blocks.length === 0}
  <span class="md">{@html fallbackHtml}</span>
{:else}
  {#each blocks as b (b.id)}
    <SpeechInline
      block={b}
      {onblockchange}
      {autoRender}
      {autoPlayOrd}
      {onAutoAdvance}
      {onAutoInterrupt}
      {agentId}
    />
    <!-- Zero-width space between adjacent pills so the browser breaks
         between them if they don't fit on one line. -->
    {' '}
  {/each}
{/if}

<style>
  .md :global(p) { margin: 0.15em 0; }
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
