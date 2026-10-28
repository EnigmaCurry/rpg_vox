<script>
  import { command, room } from '../lib/room.svelte.js';
  import { hue } from '../lib/color.js';

  // key: the strip's name in the mixer; label: what to show.
  let { key, label, online = true, mic = false, speaking = false, agent = false } = $props();

  const strip = $derived(room.strips[key] ?? { gain: 1, mute: false });
  const level = $derived(room.levels[key] ?? 0);
  // Meter in dB, -60..0 mapped to 0..100%.
  const pct = $derived(level > 0 ? Math.max(0, Math.min(100, ((20 * Math.log10(level) + 60) / 60) * 100)) : 0);
  const db = $derived(strip.gain > 0 ? `${(20 * Math.log10(strip.gain)).toFixed(1)} dB` : '-∞');

  function gain(e) {
    command({ type: 'strip', name: key, gain: Number(e.currentTarget.value) });
  }
  function mute() {
    command({ type: 'strip', name: key, mute: !strip.mute });
  }
</script>

<div class="strip" class:offline={!online} class:speaking style="--h: {agent ? 270 : hue(label)}">
  <div class="top">
    <span class="dot" title={online ? 'here' : 'away'}></span>
    <span class="label">{label}{#if label === room.you}<em>&nbsp;(you)</em>{/if}</span>
    <span class="state">{agent ? room.agent.state : speaking ? 'talking' : mic ? 'mic on' : online ? 'no mic' : 'away'}</span>
  </div>
  <div class="meter"><div class="fill" style="width: {pct}%"></div></div>
  <div class="controls">
    <input type="range" min="0" max="2" step="0.01" value={strip.gain} oninput={gain}
      ondblclick={() => command({ type: 'strip', name: key, gain: 1 })} aria-label="{label} level" />
    <span class="db">{db}</span>
    <button class:on={strip.mute} onclick={mute} aria-pressed={strip.mute}>{strip.mute ? 'Muted' : 'Mute'}</button>
  </div>
</div>

<style>
  .strip { border: 1px solid var(--line); border-radius: 10px; padding: 10px 12px; background: var(--panel); }
  .strip.speaking { border-color: hsl(var(--h) 60% 55%); box-shadow: 0 0 0 2px hsl(var(--h) 60% 55% / 25%); }
  .strip.offline { opacity: 0.55; }
  .top { display: flex; align-items: center; gap: 8px; }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--muted); }
  .strip:not(.offline) .dot { background: var(--ok); }
  .label { font-weight: 600; flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .label em { font-weight: 400; color: var(--muted); font-style: normal; }
  .state { font-size: 12px; color: var(--muted); }
  .meter { height: 6px; border-radius: 3px; background: var(--line); margin: 8px 0; overflow: hidden; }
  .fill { height: 100%; background: var(--meter); transition: width 80ms linear; }
  .controls { display: flex; align-items: center; gap: 8px; }
  .controls input { flex: 1; min-width: 0; }
  .db { font-size: 12px; color: var(--muted); width: 4.5em; text-align: right; font-variant-numeric: tabular-nums; }
  .controls button { padding: 2px 10px; font-size: 13px; }
  .controls button.on { background: var(--danger); color: #fff; border-color: var(--danger); }
</style>
