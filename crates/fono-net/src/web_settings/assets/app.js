/* Fono settings — schema-driven section renderer + two-way config binding.
   Ported from the 2026-07-02 design handoff. Vanilla JS, no framework.

   State model: `cfg` is the working copy of the daemon's config JSON
   (GET /api/config); `orig` is the last-saved snapshot. Controls carry
   `data-bind="dotted.path"` + `data-kind`; a delegated change handler
   writes values back into `cfg`, the unsaved bar shows the live diff
   count, Save PUTs the whole object back. Secrets are write-only:
   PUT /api/secret/{NAME} immediately, never rendered. */
'use strict';

// ---------- state ----------
let cfg = null, orig = null, meta = null;
// Personal vocabulary (separate resource: GET/PUT /api/vocabulary).
// `null` means it failed to load (malformed file) — editing is disabled
// so a Save can never clobber a file the user needs to fix by hand.
let vocab = null, vocabOrig = null;
const TOKEN = new URLSearchParams(location.search).get('token') || '';

async function api(path, opts = {}) {
  const headers = {};
  if (TOKEN) headers['Authorization'] = 'Bearer ' + TOKEN;
  if (opts.body) headers['Content-Type'] = 'application/json';
  const r = await fetch(path, Object.assign({}, opts, { headers }));
  if (!r.ok) {
    let m = 'HTTP ' + r.status;
    try { m = (await r.json()).error || m; } catch (e) { /* keep */ }
    throw new Error(m);
  }
  return r.json();
}

// ---------- path helpers ----------
function get(o, p) { return p.split('.').reduce((a, k) => (a == null ? undefined : a[k]), o); }
function set(o, p, v) {
  const ks = p.split('.');
  let a = o;
  for (let i = 0; i < ks.length - 1; i++) {
    if (a[ks[i]] == null || typeof a[ks[i]] !== 'object') a[ks[i]] = /^\d+$/.test(ks[i + 1]) ? [] : {};
    a = a[ks[i]];
  }
  a[ks[ks.length - 1]] = v;
}
// Value with default for keys the server omits (serde skip_serializing_if).
function gv(p, dflt) { const v = get(cfg, p); return v === undefined ? dflt : v; }
const clone = (o) => JSON.parse(JSON.stringify(o));
function esc(s) {
  return String(s == null ? '' : s).replace(/[&<>"']/g, (c) => (
    { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

// Count differing leaves between the saved and working configs.
function diffLeaves(a, b, out, pre) {
  if (a === b) return;
  const isObj = (x) => x != null && typeof x === 'object' && !Array.isArray(x);
  if (isObj(a) && isObj(b)) {
    const keys = new Set(Object.keys(a).concat(Object.keys(b)));
    keys.forEach((k) => diffLeaves(a[k], b[k], out, pre ? pre + '.' + k : k));
    return;
  }
  if (JSON.stringify(a) !== JSON.stringify(b)) out.push(pre || '(root)');
}
function dirtyPaths() { const out = []; diffLeaves(orig, cfg, out, ''); return out; }
function vocabDirty() {
  return vocab != null && JSON.stringify(vocab) !== JSON.stringify(vocabOrig);
}

// ---------- provider metadata (mirrors fono-core providers.rs) ----------
const ENV = {
  groq: 'GROQ_API_KEY', deepgram: 'DEEPGRAM_API_KEY', openai: 'OPENAI_API_KEY',
  cartesia: 'CARTESIA_API_KEY', assemblyai: 'ASSEMBLYAI_API_KEY', azure: 'AZURE_API_KEY',
  speechmatics: 'SPEECHMATICS_API_KEY', google: 'GOOGLE_API_KEY', nemotron: 'NEMOTRON_API_KEY',
  elevenlabs: 'ELEVENLABS_API_KEY', gemini: 'GEMINI_API_KEY', openrouter: 'OPENROUTER_API_KEY',
  anthropic: 'ANTHROPIC_API_KEY', cerebras: 'CEREBRAS_API_KEY',
};
// Sublabels are the exact per-role default models from
// fono-core provider_catalog.rs — keep them in sync.
const STT_PROVIDERS = [
  ['groq', 'Groq', 'whisper-large-v3-turbo'], ['deepgram', 'Deepgram', 'nova-3'],
  ['openai', 'OpenAI', 'whisper-1'], ['gemini', 'Gemini', 'gemini-flash-lite-latest'],
  ['elevenlabs', 'ElevenLabs', 'scribe_v1'], ['speechmatics', 'Speechmatics', 'enhanced'],
  ['cartesia', 'Cartesia', 'ink-whisper'], ['assemblyai', 'AssemblyAI', 'best'],
  ['azure', 'Azure', 'whisper'], ['google', 'Google', 'default'],
  ['nemotron', 'Nemotron', 'whisper-large-v3'], ['openrouter', 'OpenRouter', 'whisper-large-v3-turbo'],
];
const POLISH_PROVIDERS = [
  ['local', 'Local model', 'on-device'], ['openai', 'OpenAI', 'gpt-5.4-nano'],
  ['anthropic', 'Anthropic', 'claude-haiku-4-5'], ['gemini', 'Gemini', 'gemini-flash-lite-latest'],
  ['groq', 'Groq', 'gpt-oss-120b'], ['cerebras', 'Cerebras', 'gpt-oss-120b'],
  ['openrouter', 'OpenRouter', 'gpt-5.4-nano'], ['ollama', 'Ollama', 'localhost'],
];
const ASSISTANT_PROVIDERS = [
  ['openai', 'OpenAI', 'gpt-5.4-mini'], ['anthropic', 'Anthropic', 'claude-haiku-4-5'],
  ['gemini', 'Gemini', 'gemini-flash-lite-latest'], ['groq', 'Groq', 'gpt-oss-120b'],
  ['cerebras', 'Cerebras', 'zai-glm-4.7'], ['openrouter', 'OpenRouter', 'claude-haiku-4.5'],
  ['ollama', 'Ollama', 'localhost'],
];
const TTS_PROVIDERS = [
  ['openai', 'OpenAI', 'tts-1'], ['elevenlabs', 'ElevenLabs', 'eleven_v3'],
  ['cartesia', 'Cartesia', 'sonic-3.5'], ['deepgram', 'Deepgram', 'aura-2-thalia-en'],
  ['groq', 'Groq', 'orpheus-v1-english'], ['gemini', 'Gemini', 'flash-tts-preview'],
  ['speechmatics', 'Speechmatics', 'preview'], ['openrouter', 'OpenRouter', 'grok-voice-tts-1.0'],
];
// Cloud-only provider grids for Cleanup and the Assistant. The
// embedded local model and the Ollama / OpenAI-compatible network
// server are their own segments (Local / Network), so they are
// filtered out of the "Cloud" provider cards here.
const POLISH_CLOUD_PROVIDERS = POLISH_PROVIDERS.filter((p) => p[0] !== 'local' && p[0] !== 'ollama');
const ASSISTANT_CLOUD_PROVIDERS = ASSISTANT_PROVIDERS.filter((p) => p[0] !== 'ollama');
// Default endpoint offered when switching Cleanup / Assistant to the
// Network (self-hosted server) segment.
const LOCAL_SERVER_URL = 'http://localhost:11434/v1/chat/completions';
const OVERLAY_STYLES = [
  ['bars', 'Bars', 'p-bars', ''], ['oscilloscope', 'Oscilloscope', 'p-osc', ''],
  ['fft', 'FFT', 'p-fft', ''], ['heatmap', 'Heatmap', 'p-heat', ''],
  ['terrain3d', '3D Terrain', 'p-terr', ''], ['system360', 'System/360', 'p-dots', ''],
  ['cortex', 'Glass Cortex', 'p-cortex', ''],
  ['transcript', 'Transcript', 'p-text', 'more CPU/API'],
];
function pname(list, id) { const p = list.find((x) => x[0] === id); return p ? p[1] : id; }
function pdef(list, id) { const p = list.find((x) => x[0] === id); return p ? p[2] : ''; }

// Glass Cortex preview: a flat-cell LED activation matrix rendered as an
// inline SVG so every cell is one solid colour (not a smooth gradient),
// mirroring the live cortex renderer. Colours step through the warm
// "compute" ramp (fono-overlay cortex.rs RAMP_WARM: #1a0c22 → #782860 →
// #d9342f → #ff8b5e → #fff7ec); a dense hot cluster on the left fades to
// idle dim cells on the right. Generated once at load, injected as the
// preview tile's background.
const CORTEX_RAMP = [
  '#241021', '#3a1730', '#5c1f4c', '#782860', '#b0332c',
  '#d9342f', '#ff8b5e', '#ffe6cf', '#fff7ec',
];
function cortexMatrixBg() {
  const cols = 18, rows = 4, pitch = 10, cell = 8.4, rx = 1.3;
  let rects = '';
  for (let r = 0; r < rows; r++) {
    for (let c = 0; c < cols; c++) {
      const x = c / (cols - 1);
      // Deterministic per-cell jitter so the pattern is stable.
      const n = Math.sin(c * 127.1 + r * 311.7) * 43758.5453;
      const rnd = n - Math.floor(n);
      // Hot, busy cluster on the left third; dim idle field on the right.
      let v = x < 0.6 ? 0.55 + rnd * 0.5 - x * 0.55 : 0.05 + rnd * 0.2;
      v = Math.max(0, Math.min(1, v));
      const fill = CORTEX_RAMP[Math.round(v * (CORTEX_RAMP.length - 1))];
      rects += '<rect x="' + (c * pitch) + '" y="' + (r * pitch) + '" width="'
        + cell + '" height="' + cell + '" rx="' + rx + '" fill="' + fill + '"/>';
    }
  }
  const w = (cols - 1) * pitch + cell, h = (rows - 1) * pitch + cell;
  return 'data:image/svg+xml,' + encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ' + w + ' ' + h + '">' + rects + '</svg>');
}
const CORTEX_BG = cortexMatrixBg();

// ---------- control builders ----------
function row(lbl, desc, ctl, cls) {
  return '<div class="row' + (cls ? ' ' + cls : '') + '"><div class="info"><div class="lbl">' + esc(lbl) + '</div>'
    + (desc ? '<div class="desc">' + desc + '</div>' : '') + '</div><div class="ctl">' + ctl + '</div></div>';
}
function toggle(path, dflt, rr) {
  return '<input type="checkbox" class="toggle" data-bind="' + path + '" data-kind="toggle"'
    + (rr ? ' data-rr="' + rr + '"' : '') + (gv(path, dflt) ? ' checked' : '') + ' />';
}
function txt(path, o) {
  o = o || {};
  return '<input class="input' + (o.mono ? ' mono' : '') + '" data-bind="' + path + '" data-kind="text" value="'
    + esc(gv(path, o.dflt || '')) + '"' + (o.ph ? ' placeholder="' + esc(o.ph) + '"' : '')
    + (o.w ? ' style="width:' + o.w + 'px"' : '') + ' />';
}
function num(path, dflt, unit) {
  return '<input class="input sm mono" data-bind="' + path + '" data-kind="num" value="' + gv(path, dflt) + '" />'
    + (unit ? ' <span class="hint">' + unit + '</span>' : '');
}
function flt(path, dflt, unit) {
  return '<input class="input sm mono" data-bind="' + path + '" data-kind="float" value="' + gv(path, dflt) + '" />'
    + (unit ? ' <span class="hint">' + unit + '</span>' : '');
}
function sel(path, opts, dflt, rr) {
  const cur = gv(path, dflt);
  return '<select class="select" data-bind="' + path + '" data-kind="text"' + (rr ? ' data-rr="' + rr + '"' : '') + '>'
    + opts.map((o) => '<option value="' + esc(o[0]) + '"' + (o[0] === cur ? ' selected' : '') + '>' + esc(o[1]) + '</option>').join('')
    + '</select>';
}
function tags(path, ph) {
  const items = gv(path, []) || [];
  return '<div class="tags" data-tags="' + path + '">'
    + items.map((t, i) => '<span class="tag">' + esc(t) + ' <button class="x" type="button" data-tag-rm="' + i + '" aria-label="Remove">&times;</button></span>').join('')
    + '<input class="ghost" placeholder="' + esc(ph || 'Add\u2026') + '" /></div>';
}
function keycap(path) {
  return '<button class="keycap" type="button" data-keycap="' + path + '">' + esc(gv(path, '')) + '</button>';
}
function seg(group, opts, cur) {
  return '<div class="seg">' + opts.map((o) =>
    '<button type="button" data-seg="' + group + '" data-val="' + o[0] + '" aria-pressed="' + (o[0] === cur) + '">' + esc(o[1]) + '</button>').join('') + '</div>';
}
function pgrid(list, pick, cur, extra) {
  return '<div class="pgrid' + (extra || '') + '">' + list.map((p) =>
    '<button type="button" class="pcard" data-pick="' + pick + '" data-val="' + p[0] + '" aria-pressed="' + (p[0] === cur) + '">'
    + '<div class="pname">' + esc(p[1]) + '</div><div class="pmeta">' + esc(p[2]) + '</div></button>').join('') + '</div>';
}
function ovgrid(cur) {
  return '<div class="pgrid ovgrid">' + OVERLAY_STYLES.map((s) => {
    // The cortex tile is a generated flat-cell matrix, not a CSS pattern.
    const style = s[0] === 'cortex'
      ? ' style="background:#140a16 url(&quot;' + CORTEX_BG + '&quot;) center/100% 100% no-repeat"'
      : '';
    return '<button type="button" class="pcard ov" data-pick="overlay-style" data-val="' + s[0] + '" aria-pressed="' + (s[0] === cur) + '">'
      + '<div class="ovprev ' + s[2] + '"' + style + '></div><div class="pname">' + esc(s[1]) + '</div>'
      + (s[3] ? '<div class="pmeta">' + esc(s[3]) + '</div>' : '') + '</button>';
  }).join('') + '</div>';
}
// Write-only secret status + set/replace/clear. `env` is the secret name.
function keyRow(env, lbl, desc) {
  if (!env) return '';
  const isSet = !!(meta && meta.secrets && meta.secrets[env]);
  const ctl = isSet
    ? '<span class="keystatus"><span class="dot"></span>Configured \u2713</span>'
      + '<button class="btn" type="button" data-key-edit="' + env + '">Replace\u2026</button>'
      + '<button class="btn ghost" type="button" data-key-clear="' + env + '">Clear</button>'
    : '<span class="keystatus unset"><span class="dot"></span>Not set</span>'
      + '<button class="btn" type="button" data-key-edit="' + env + '">Set key\u2026</button>';
  return row(lbl || 'API key', (desc || 'Write-only \u2014 stored value is never shown.')
    + ' <span class="mono hint">' + esc(env) + '</span>', ctl);
}
function promptRow(lbl, hint, path, dkey, rows) {
  return '<details class="prompt-d"><summary><span class="lbl">' + esc(lbl) + '</span><span class="hint">' + esc(hint) + '</span>'
    + '<span style="margin-left:auto" class="hint">edit \u25be</span></summary>'
    + '<textarea class="input mono" rows="' + (rows || 5) + '" data-bind="' + path + '" data-kind="text">' + esc(gv(path, '')) + '</textarea>'
    + (dkey ? '<button class="btn ghost" type="button" data-reset="' + path + '" data-dkey="' + dkey + '">Reset to default</button>' : '')
    + '</details>';
}
function srvCard(title, inner, togglePath, rr) {
  return '<div class="srv-card"><div class="srv-h"><span class="lbl">' + esc(title) + '</span>'
    + toggle(togglePath, false, rr) + '</div><div class="srv-grid">' + inner + '</div></div>';
}
function srvField(lbl, inner) { return '<label>' + esc(lbl) + ' ' + inner + '</label>'; }
function srvInput(path, dflt, ph) {
  return '<input class="input mono" data-bind="' + path + '" data-kind="text" value="' + esc(gv(path, dflt)) + '"'
    + (ph ? ' placeholder="' + esc(ph) + '"' : '') + ' />';
}
function srvNum(path, dflt) {
  return '<input class="input mono" data-bind="' + path + '" data-kind="num" value="' + gv(path, dflt) + '" />';
}

// ---------- optional sub-table constructors ----------
function ensureSttCloud(provider) {
  if (!cfg.stt.cloud || typeof cfg.stt.cloud !== 'object') cfg.stt.cloud = { provider: '', api_key_ref: '', model: '' };
  // A model name is provider-specific: drop it when the provider changes.
  if (get(cfg, 'stt.cloud.provider') !== provider) set(cfg, 'stt.cloud.model', '');
  set(cfg, 'stt.cloud.provider', provider);
  set(cfg, 'stt.cloud.api_key_ref', ENV[provider] || '');
  if (get(cfg, 'stt.cloud.model') === undefined) set(cfg, 'stt.cloud.model', '');
}
function ensureCloud(base, provider) {
  if (!get(cfg, base) || typeof get(cfg, base) !== 'object') set(cfg, base, { provider: '', api_key_ref: '', model: '' });
  // A model name is provider-specific: drop it when the provider changes.
  if (get(cfg, base + '.provider') !== provider) set(cfg, base + '.model', '');
  set(cfg, base + '.provider', provider);
  set(cfg, base + '.api_key_ref', ENV[provider] || '');
  if (get(cfg, base + '.model') === undefined) set(cfg, base + '.model', '');
}

// ---------- derived segment state ----------
function sttSeg() {
  const b = gv('stt.backend', 'local');
  return b === 'local' ? 'local' : b === 'wyoming' ? 'wyoming' : 'cloud';
}
function ttsSeg() {
  const b = gv('tts.backend', 'none');
  return b === 'none' || b === 'local' || b === 'wyoming' ? b : 'cloud';
}
function astopSeg() {
  const ms = gv('audio.auto_stop_silence_ms', 3000);
  return ms === 0 ? 'off' : ms === 3000 ? '3000' : ms === 5000 ? '5000' : 'custom';
}
// Cleanup backend → segment. `local` = embedded model, `ollama` =
// self-hosted server (Network), anything else = a cloud provider.
function polishSeg() {
  const b = gv('polish.backend', 'local');
  return b === 'local' ? 'local' : b === 'ollama' ? 'network' : 'cloud';
}
// Assistant backend → segment. The embedded model and a self-hosted
// server share the `ollama` backend; they are told apart by the
// `[assistant.cloud].provider` marker (`ollama-server` /
// `openai-compatible-local` = Network, otherwise embedded Local).
function assistantIsNetwork() {
  const p = gv('assistant.cloud.provider', '');
  return p === 'ollama-server' || p === 'openai-compatible-local';
}
function assistantSeg() {
  const b = gv('assistant.backend', 'none');
  if (b === 'ollama') return assistantIsNetwork() ? 'network' : 'local';
  return 'cloud';
}
// Embedded local-LLM panel for the Cleanup / Assistant "Local" segment.
// Shows the current on-device GGUF model id and lets the user change it.
// `base` is 'polish' or 'assistant'.
function localLlmPanel(base) {
  return row('Model', 'Embedded on-device model. Install others with <span class="mono">fono models install &lt;id&gt;</span>.',
    txt(base + '.local.model', { mono: true, w: 220, ph: 'gemma-4-e2b' }))
    + '<p class="hint" style="margin-top:6px">Runs on this machine \u2014 no API key, nothing leaves your computer.</p>';
}

// ---------- local TTS engine + voice picker ----------
// Renders the engine card row (auto/piper/kokoro/supertonic from
// /api/meta) plus a per-engine preset-voice dropdown, falling back to a
// free-text catalog-id field for `auto` (which spans the whole catalog).
function ttsLocalPanel() {
  const engines = (meta && meta.tts_local && meta.tts_local.engines) || [];
  const eng = gv('tts.local.engine', 'auto');
  let out = '';
  if (engines.length) {
    const cards = engines.map((e) =>
      [e.id, e.label, (e.voices && e.voices.length) ? e.voices.length + ' voices' : 'language-aware']);
    out += '<div class="subhead">Engine</div>' + pgrid(cards, 'tts-local-engine', eng);
  }
  const cur = engines.find((e) => e.id === eng);
  if (cur && cur.voices && cur.voices.length) {
    const opts = [['', 'Default / auto']].concat(cur.voices.map((v) => {
      const bits = [v.language, v.gender].filter((x) => x && x !== 'multi' && x !== 'neutral');
      return [v.id, bits.length ? v.id + ' \u00b7 ' + bits.join(' \u00b7 ') : v.id];
    }));
    out += row('Voice', 'Preset voices for this engine.', sel('tts.local.voice', opts, ''));
  } else {
    out += row('Voice', 'Catalog voice id, e.g. en_US-lessac-medium. Empty = match your first language.',
      txt('tts.local.voice', { mono: true, w: 220, ph: 'auto' }));
  }
  return out + row('Test', 'Plays through your browser.', ttsTestBox('local'));
}

// Inline "type a sentence and hear it" tester. `kind` picks how the
// click handler resolves the route (local engine vs configured cloud
// provider vs Wyoming). Ephemeral — not bound into cfg. The typed text
// lives in `ttsSample` (module-level) so it survives the section
// re-render that a voice/engine pick triggers, instead of snapping back
// to the default sentence.
let ttsSample = 'The quick brown fox jumps over the lazy dog.';
function ttsTestBox(kind) {
  return '<div class="ttstest">'
    + '<input class="input tts-sample" placeholder="Type a sentence to hear it\u2026" '
    + 'value="' + esc(ttsSample) + '" />'
    + '<button class="btn" type="button" data-tts-test="' + kind + '">Test voice</button>'
    + '<span class="hint tts-status"></span></div>';
}

// Synthesize via the OpenAI-compatible POST /v1/audio/speech endpoint and
// play the returned WAV through the Web Audio API — so playback happens in
// the browser even when the daemon runs on a remote box. This same Web
// Audio primitive is what the future assistant page will build on for mic
// capture + streamed audio.
let ttsAudioCtx = null;
async function playSpeech(model, voice, input, statusEl) {
  if (statusEl) statusEl.textContent = 'Synthesizing\u2026';
  try {
    const headers = { 'Content-Type': 'application/json' };
    if (TOKEN) headers['Authorization'] = 'Bearer ' + TOKEN;
    const body = { model: model || undefined, input: input, response_format: 'wav' };
    if (voice) body.voice = voice;
    const r = await fetch('/v1/audio/speech', { method: 'POST', headers, body: JSON.stringify(body) });
    if (!r.ok) {
      let m = 'HTTP ' + r.status;
      try { const j = await r.json(); m = (j.error && (j.error.message || j.error)) || m; } catch (e) { /* keep */ }
      throw new Error(m);
    }
    const buf = await r.arrayBuffer();
    ttsAudioCtx = ttsAudioCtx || new (window.AudioContext || window.webkitAudioContext)();
    if (ttsAudioCtx.state === 'suspended') await ttsAudioCtx.resume();
    const audio = await ttsAudioCtx.decodeAudioData(buf);
    const src = ttsAudioCtx.createBufferSource();
    src.buffer = audio;
    src.connect(ttsAudioCtx.destination);
    src.start();
    if (statusEl) statusEl.textContent = 'Playing \u00b7 ' + audio.duration.toFixed(1) + 's';
  } catch (e) {
    if (statusEl) statusEl.textContent = 'Error: ' + e.message;
  }
}

// Segment click handlers: value -> mutate cfg; section is re-rendered.
const SEG = {
  stt(v) {
    if (v === 'local') set(cfg, 'stt.backend', 'local');
    else if (v === 'wyoming') {
      if (!get(cfg, 'stt.wyoming')) set(cfg, 'stt.wyoming', { uri: '' });
      set(cfg, 'stt.backend', 'wyoming');
    } else {
      const p = gv('stt.cloud.provider', '') || 'groq';
      ensureSttCloud(p);
      set(cfg, 'stt.backend', p);
    }
  },
  tts(v) {
    if (v === 'cloud') {
      const p = gv('tts.cloud.provider', '') || 'openai';
      ensureCloud('tts.cloud', p);
      set(cfg, 'tts.backend', p);
    } else {
      if (v === 'wyoming' && !get(cfg, 'tts.wyoming')) set(cfg, 'tts.wyoming', { uri: '' });
      set(cfg, 'tts.backend', v);
    }
  },
  astop(v) {
    if (v === 'off') set(cfg, 'audio.auto_stop_silence_ms', 0);
    else if (v === 'custom') { if (gv('audio.auto_stop_silence_ms', 0) === 0) set(cfg, 'audio.auto_stop_silence_ms', 4000); }
    else set(cfg, 'audio.auto_stop_silence_ms', parseInt(v, 10));
  },
  polish(v) {
    if (v === 'local') set(cfg, 'polish.backend', 'local');
    else if (v === 'network') {
      if (!get(cfg, 'polish.cloud') || typeof get(cfg, 'polish.cloud') !== 'object') {
        set(cfg, 'polish.cloud', { provider: '', api_key_ref: '', model: '' });
      }
      // Switching in from a cloud provider: seed the endpoint + drop the
      // provider-specific model. Preserve both when already on the server.
      if (gv('polish.cloud.provider', '') !== 'ollama') {
        set(cfg, 'polish.cloud.model', '');
        set(cfg, 'polish.cloud.api_key_ref', LOCAL_SERVER_URL);
      }
      set(cfg, 'polish.cloud.provider', 'ollama');
      set(cfg, 'polish.backend', 'ollama');
    } else {
      const prev = gv('polish.cloud.provider', '');
      const p = POLISH_CLOUD_PROVIDERS.some((x) => x[0] === prev) ? prev : 'openai';
      ensureCloud('polish.cloud', p);
      set(cfg, 'polish.cloud.provider', p);
      set(cfg, 'polish.cloud.api_key_ref', ENV[p] || '');
      set(cfg, 'polish.backend', p);
    }
  },
  assistant(v) {
    if (v === 'local') {
      // Embedded on-device model = Ollama backend with no manual server.
      // Clear the server markers so the factory takes the embedded path.
      if (get(cfg, 'assistant.cloud')) {
        set(cfg, 'assistant.cloud.provider', '');
        set(cfg, 'assistant.cloud.api_key_ref', '');
        set(cfg, 'assistant.cloud.model', '');
      }
      set(cfg, 'assistant.backend', 'ollama');
    } else if (v === 'network') {
      if (!get(cfg, 'assistant.cloud') || typeof get(cfg, 'assistant.cloud') !== 'object') {
        set(cfg, 'assistant.cloud', { provider: '', api_key_ref: '', model: '' });
      }
      if (!assistantIsNetwork()) {
        set(cfg, 'assistant.cloud.model', '');
        set(cfg, 'assistant.cloud.api_key_ref', LOCAL_SERVER_URL);
      }
      set(cfg, 'assistant.cloud.provider', 'ollama-server');
      set(cfg, 'assistant.backend', 'ollama');
    } else {
      const prev = gv('assistant.cloud.provider', '');
      const p = ASSISTANT_CLOUD_PROVIDERS.some((x) => x[0] === prev) ? prev : 'openai';
      ensureCloud('assistant.cloud', p);
      set(cfg, 'assistant.cloud.provider', p);
      set(cfg, 'assistant.cloud.api_key_ref', ENV[p] || '');
      set(cfg, 'assistant.backend', p);
    }
  },
};

// Provider-card click handlers. The explicit `.provider` / `.api_key_ref`
// sets duplicate ensureCloud's work on purpose: the coverage test in
// web_settings/mod.rs greps this file for full dotted paths.
const PICK = {
  'stt-provider'(v) { ensureSttCloud(v); set(cfg, 'stt.backend', v); },
  'polish-provider'(v) {
    ensureCloud('polish.cloud', v);
    set(cfg, 'polish.cloud.provider', v);
    set(cfg, 'polish.cloud.api_key_ref', ENV[v] || '');
    set(cfg, 'polish.backend', v);
  },
  'assistant-provider'(v) {
    set(cfg, 'assistant.backend', v);
    ensureCloud('assistant.cloud', v);
    set(cfg, 'assistant.cloud.provider', v);
    set(cfg, 'assistant.cloud.api_key_ref', ENV[v] || '');
  },
  'tts-provider'(v) {
    ensureCloud('tts.cloud', v);
    set(cfg, 'tts.cloud.provider', v);
    set(cfg, 'tts.cloud.api_key_ref', ENV[v] || '');
    set(cfg, 'tts.backend', v);
  },
  'tts-local-engine'(v) {
    // Preset voices differ per engine, so drop a stale cross-engine
    // voice pin when switching (keeps the dropdown consistent).
    if (gv('tts.local.engine', 'auto') !== v) set(cfg, 'tts.local.voice', '');
    set(cfg, 'tts.local.engine', v);
  },
  'overlay-style'(v) { set(cfg, 'overlay.style', v); },
};

// ---------- sections ----------
const FONO_SECTIONS = [
  {
    id: 'general', title: 'General', rr: false,
    summary() {
      const langs = gv('general.languages', []);
      return (langs.length ? langs.join(', ') : 'auto-detect')
        + (gv('general.startup_autostart', false) ? ' \u00b7 starts on login' : '');
    },
    html() {
      return row('Languages', 'Language codes to transcribe (e.g. en, sv). Empty = auto-detect all languages.',
        tags('general.languages', 'Add language\u2026'))
        + row('Start on login', 'Launch Fono in the background when you sign in.', toggle('general.startup_autostart', false))
        + row('Also copy result to clipboard', 'In addition to typing the transcript at the cursor.', toggle('general.also_copy_to_clipboard', false))
        + row('Mute system audio while recording', 'Prevents music or video audio from bleeding into the mic.', toggle('general.auto_mute_system', true));
    },
  },
  {
    id: 'hotkeys', title: 'Hotkeys & Wake Word',
    summary() {
      let s = gv('hotkeys.dictation', 'F7') + ' \u00b7 ' + gv('hotkeys.assistant', 'F8');
      const ph = gv('wakeword.phrases', []);
      if (gv('wakeword.enabled', false) && ph.length) s += ' \u00b7 \u201c' + ph[0].model.replace(/_/g, ' ') + '\u201d';
      return s;
    },
    html() {
      const phrases = gv('wakeword.phrases', []) || [];
      const wakeOn = gv('wakeword.enabled', false);
      const rows = phrases.map((p, i) =>
        '<div class="wake-row">'
        + '<div><input class="input mono" data-bind="wakeword.phrases.model" data-idx="' + i + '" data-kind="text" value="' + esc(p.model) + '" style="width:170px" /></div>'
        + '<div class="ctl"><span class="hint sens">' + Number(p.sensitivity).toFixed(2) + '</span>'
        + '<input type="range" class="slider" min="0" max="1" step="0.01" data-bind="wakeword.phrases.sensitivity" data-idx="' + i + '" data-kind="float" value="' + p.sensitivity + '" /></div>'
        + '<div class="radio-pair">'
        + '<label><input type="radio" name="wk' + i + '" value="dictation" data-bind="wakeword.phrases.target" data-idx="' + i + '" data-kind="radio"' + (p.target === 'dictation' ? ' checked' : '') + ' />Dictation</label>'
        + '<label><input type="radio" name="wk' + i + '" value="assistant" data-bind="wakeword.phrases.target" data-idx="' + i + '" data-kind="radio"' + (p.target === 'assistant' ? ' checked' : '') + ' />Assistant</label></div>'
        + '<button class="btn ghost" type="button" data-wake-rm="' + i + '">Remove</button></div>').join('');
      return keycapRow('Dictation key', 'Short press toggles \u00b7 hold for push-to-talk.', 'hotkeys.dictation')
        + keycapRow('Assistant key', 'Ask a question by voice instead of dictating.', 'hotkeys.assistant')
        + keycapRow('Cancel key', 'Discard the current recording.', 'hotkeys.cancel')
        + row('Wake word', 'Listen for a spoken phrase to start recording.', toggle('wakeword.enabled', false, 'hotkeys'), 'master')
        + '<div' + (wakeOn ? '' : ' class="section-off"') + '>'
        + rows
        + '<div class="row master" style="border:0;padding-top:10px;"><div class="ctl"><button class="btn" type="button" data-wake-add>+ Add wake phrase</button></div></div>'
        + '</div>';
    },
  },
  {
    id: 'stt', title: 'Speech to Text', rr: true,
    summary() {
      const s = sttSeg();
      if (s === 'local') return 'Local \u00b7 whisper ' + gv('stt.local.model', 'small');
      if (s === 'wyoming') return 'Network \u00b7 ' + (gv('stt.wyoming.uri', '') || 'no server');
      const p = gv('stt.backend', '');
      const env = ENV[p];
      const keySet = env && meta && meta.secrets && meta.secrets[env];
      return 'Cloud \u00b7 ' + pname(STT_PROVIDERS, p) + (keySet ? ' \u00b7 key set \u2713' : ' \u00b7 no key');
    },
    html() {
      const s = sttSeg();
      let panel = '';
      if (s === 'local') {
        panel = row('Model', 'Bigger models are more accurate but slower.',
          sel('stt.local.model', [['tiny', 'tiny'], ['base', 'base'], ['small', 'small'], ['medium', 'medium'], ['large', 'large']], 'small'))
          + row('Quantization', '', sel('stt.local.quantization', [['auto', 'auto'], ['int8', 'int8'], ['fp16', 'fp16']], 'auto'));
      } else if (s === 'wyoming') {
        panel = row('Server URI', 'Wyoming protocol \u2014 e.g. tcp://10.0.0.4:10300.', txt('stt.wyoming.uri', { mono: true, w: 240 }))
          + row('Model hint', 'Optional; empty lets the server pick.', txt('stt.wyoming.model', { mono: true, w: 180 }));
      } else {
        const p = gv('stt.backend', 'groq');
        panel = '<div class="subhead">Provider</div>' + pgrid(STT_PROVIDERS, 'stt-provider', p)
          + '<div style="margin-top:12px">'
          + row('Model', 'Empty = provider default.', txt('stt.cloud.model', { mono: true, w: 240, ph: pdef(STT_PROVIDERS, p) }))
          + keyRow(ENV[p]) + '</div>';
      }
      return row('Backend', 'Local runs on this machine. Network connects to a Wyoming server.',
        seg('stt', [['local', 'Local'], ['cloud', 'Cloud'], ['wyoming', 'Network']], s)) + panel;
    },
  },
  {
    id: 'cleanup', title: 'Cleanup',
    summary() {
      if (!gv('polish.enabled', false)) return 'Off';
      const s = polishSeg();
      if (s === 'local') return 'Local model';
      if (s === 'network') return 'Network \u00b7 ' + (gv('polish.cloud.api_key_ref', '') || 'no server');
      return 'Cloud \u00b7 ' + pname(POLISH_CLOUD_PROVIDERS, gv('polish.backend', ''));
    },
    html() {
      const on = gv('polish.enabled', false);
      const s = polishSeg();
      let panel = '';
      if (s === 'local') {
        panel = localLlmPanel('polish');
      } else if (s === 'network') {
        panel = row('Server URL', 'Ollama / OpenAI-compatible endpoint \u2014 e.g. http://localhost:11434/v1/chat/completions.',
          txt('polish.cloud.api_key_ref', { mono: true, w: 300 }))
          + row('Model', 'Model id served by that endpoint.', txt('polish.cloud.model', { mono: true, w: 220, ph: 'gemma4:12b' }));
      } else {
        const b = gv('polish.backend', 'openai');
        panel = '<div class="subhead">Provider</div>'
          + pgrid(POLISH_CLOUD_PROVIDERS, 'polish-provider', b)
          + '<div style="margin-top:12px">'
          + row('Model', 'Empty = provider default.', txt('polish.cloud.model', { mono: true, w: 220, ph: pdef(POLISH_PROVIDERS, b) }))
          + keyRow(ENV[b]) + '</div>';
      }
      return row('Enable cleanup', 'Runs each transcript through a small language model \u2014 punctuation, casing, filler removal.',
        toggle('polish.enabled', false, 'cleanup'), 'master')
        + '<div' + (on ? '' : ' class="section-off"') + '>'
        + row('Backend', 'Local runs on this machine. Network connects to an Ollama / OpenAI-compatible server.',
          seg('polish', [['local', 'Local'], ['cloud', 'Cloud'], ['network', 'Network']], s))
        + panel
        + '<div style="margin-top:12px">'
        + row('Personal dictionary', 'Words and spellings to preserve.', tags('polish.prompt.dictionary'))
        + '</div>'
        + promptRow('Cleanup prompt', 'How transcripts are polished', 'polish.prompt.main', 'polish_prompt_main', 8)
        + promptRow('Advanced prompt', 'Extra rules appended to the system message', 'polish.prompt.advanced', 'polish_prompt_advanced', 5)
        + '</div>';
    },
  },
  {
    id: 'vocabulary', title: 'Vocabulary',
    summary() {
      if (vocab == null) return 'could not load';
      const n = (vocab.vocabulary || []).length;
      return n ? n + ' correction' + (n === 1 ? '' : 's') : 'none';
    },
    html() {
      if (vocab == null) {
        return '<p class="privacy-note">vocabulary.toml could not be read — fix or remove the file, then reload this page.</p>';
      }
      const entries = vocab.vocabulary || [];
      const rows = entries.map((en, i) =>
        '<div class="wake-row">'
        + '<div><input class="input mono" data-vocab-from="' + i + '" value="' + esc((en.from || []).join(', ')) + '" placeholder="phono, phone oh" /></div>'
        + '<div class="ctl"><span class="hint">→</span><input class="input mono" data-vocab-to="' + i + '" value="' + esc(en.to) + '" placeholder="Fono" style="width:150px" /></div>'
        + '<button class="btn ghost" type="button" data-vocab-rm="' + i + '">Remove</button></div>').join('');
      return row('Corrections', 'Deterministic fixes applied to every transcript before it reaches the cursor. '
        + 'Left: the mishearings as speech-to-text writes them (comma-separated, case-insensitive; multi-word is fine). '
        + 'Right: the spelling you want. Whole words only — a “phono” rule never touches “phonograph”. '
        + 'Active from the next dictation.', '')
        + rows
        + '<div class="row master" style="border:0;padding-top:10px;"><div class="ctl"><button class="btn" type="button" data-vocab-add>+ Add correction</button></div></div>';
    },
  },
  {
    id: 'assistant', title: 'Assistant',
    summary() {
      if (!gv('assistant.enabled', false)) return 'Off';
      const s = assistantSeg();
      let str;
      if (s === 'local') str = 'Local model';
      else if (s === 'network') str = 'Network \u00b7 ' + (gv('assistant.cloud.api_key_ref', '') || 'no server');
      else {
        const b = gv('assistant.backend', 'none');
        str = b === 'none' ? 'no backend' : pname(ASSISTANT_CLOUD_PROVIDERS, b);
      }
      if (gv('assistant.realtime.live_mode', true)) str += ' \u00b7 live mode on';
      return str;
    },
    html() {
      const on = gv('assistant.enabled', false);
      const s = assistantSeg();
      let panel = '';
      if (s === 'local') {
        panel = localLlmPanel('assistant');
      } else if (s === 'network') {
        panel = row('Server URL', 'Ollama / OpenAI-compatible endpoint \u2014 e.g. http://localhost:11434/v1/chat/completions.',
          txt('assistant.cloud.api_key_ref', { mono: true, w: 300 }))
          + row('Model', 'Model id served by that endpoint.', txt('assistant.cloud.model', { mono: true, w: 220, ph: 'gemma4:12b' }));
      } else {
        const b = gv('assistant.backend', 'openai');
        const gridB = ASSISTANT_CLOUD_PROVIDERS.some((x) => x[0] === b) ? b : '';
        panel = '<div class="subhead">Provider</div>'
          + pgrid(ASSISTANT_CLOUD_PROVIDERS, 'assistant-provider', gridB)
          + '<div style="margin-top:12px">'
          + row('Model', 'Empty = provider default.', txt('assistant.cloud.model', { mono: true, w: 220, ph: pdef(ASSISTANT_PROVIDERS, b) }))
          + keyRow(ENV[b]) + '</div>';
      }
      return row('Enable assistant', 'Voice Q&A \u2014 ask a question, hear or read the answer.',
        toggle('assistant.enabled', false, 'assistant'), 'master')
        + '<div' + (on ? '' : ' class="section-off"') + '>'
        + row('Backend', 'Local runs on this machine. Network connects to an Ollama / OpenAI-compatible server.',
          seg('assistant', [['local', 'Local'], ['cloud', 'Cloud'], ['network', 'Network']], s))
        + panel
        + promptRow('System prompt', 'Personality and constraints', 'assistant.prompt_main', 'assistant_prompt', 5)
        + row('Live conversation mode', 'A tap on the assistant key opens a continuous conversation (realtime models only).',
          toggle('assistant.realtime.live_mode', true))
        + row('Max session length', 'Hard stop for a live session. 0 = no cap.', num('assistant.realtime.max_session_secs', 300, 'seconds'))
        + row('Prefer vision-capable model', 'Lets the assistant see your screen when asked.', toggle('assistant.prefer_vision', true))
        + row('Web search', 'Allow the provider\u2019s native web-search tool.', toggle('assistant.prefer_web_search', false))
        + '<div class="subhead">Memory</div>'
        + row('History window', 'Turns older than this are forgotten.', num('assistant.history_window_minutes', 5, 'minutes'))
        + row('Max turns', '', num('assistant.history_max_turns', 12))
        + '</div>';
    },
  },
  {
    id: 'voice', title: 'Voice',
    summary() {
      const s = ttsSeg();
      if (s === 'none') return 'Off';
      if (s === 'local') return 'Local \u00b7 ' + (gv('tts.local.voice', '') || 'auto voice');
      if (s === 'wyoming') return 'Network \u00b7 ' + (gv('tts.wyoming.uri', '') || 'no server');
      return pname(TTS_PROVIDERS, gv('tts.backend', ''));
    },
    html() {
      const s = ttsSeg();
      let panel = '';
      if (s === 'local') {
        panel = ttsLocalPanel();
      } else if (s === 'cloud') {
        const p = gv('tts.backend', 'openai');
        panel = '<div class="subhead">Provider</div>' + pgrid(TTS_PROVIDERS, 'tts-provider', p)
          + '<div style="margin-top:12px">'
          + row('Model', 'Empty = provider default.', txt('tts.cloud.model', { mono: true, w: 200, ph: pdef(TTS_PROVIDERS, p) }))
          + row('Voice', 'Voice id \u2014 see `fono voices`. Empty = backend default.', txt('tts.voice', { mono: true, w: 200, ph: 'default' }))
          + keyRow(ENV[p])
          + row('Test', 'Plays through your browser.', ttsTestBox('cloud')) + '</div>';
      } else if (s === 'wyoming') {
        panel = row('Server URI', 'Wyoming protocol \u2014 e.g. tcp://10.0.0.4:10200.', txt('tts.wyoming.uri', { mono: true, w: 240 }))
          + row('Token ref', 'Optional pre-shared token reference.', txt('tts.wyoming.auth_token_ref', { mono: true, w: 180, ph: 'none' }))
          + row('Voice', 'Empty = server default.', txt('tts.voice', { mono: true, w: 200, ph: 'default' }))
          + row('Test', 'Plays through your browser.', ttsTestBox('wyoming'));
      }
      const dev = s === 'none' ? '' : row('Output device', 'Empty = system default (daemon-side playback only).', txt('tts.output_device', { w: 200, ph: 'System default' }));
      return row('Backend', 'Text-to-speech for assistant replies.',
        seg('tts', [['none', 'None'], ['local', 'Local'], ['cloud', 'Cloud'], ['wyoming', 'Network']], s)) + panel + dev;
    },
  },
  {
    id: 'overlay', title: 'Overlay & Audio',
    summary() {
      const st = OVERLAY_STYLES.find((s) => s[0] === gv('overlay.style', 'fft'));
      const ms = gv('audio.auto_stop_silence_ms', 3000);
      return (gv('overlay.waveform', true) ? (st ? st[1] : '') : 'hidden')
        + ' \u00b7 ' + (ms === 0 ? 'no auto-stop' : 'auto-stop ' + (ms % 1000 === 0 ? ms / 1000 + 's' : ms + 'ms'));
    },
    html() {
      const on = gv('overlay.waveform', true);
      const a = astopSeg();
      return row('Show overlay while recording', '', toggle('overlay.waveform', true, 'overlay'), 'master')
        + '<div' + (on ? '' : ' class="section-off"') + '>'
        + '<div class="subhead">Style</div>' + ovgrid(gv('overlay.style', 'fft'))
        + '</div>'
        + '<div style="margin-top:12px">'
        + row('Trim silence', 'Cut leading and trailing silence before transcribing.', toggle('audio.trim_silence', true))
        + row('Auto-stop after silence', 'Stops a toggle-mode recording once you go quiet.',
          seg('astop', [['off', 'Off'], ['3000', '3s'], ['5000', '5s'], ['custom', 'Custom']], a)
          + (a === 'custom' ? ' ' + num('audio.auto_stop_silence_ms', 4000, 'ms') : ''))
        + '</div>';
    },
  },
  {
    id: 'history', title: 'History & Privacy',
    summary() {
      return gv('history.enabled', true)
        ? gv('history.retention_days', 90) + ' days' + (gv('history.redact_secrets', true) ? ' \u00b7 redaction on' : '')
        : 'Off';
    },
    html() {
      return row('Save dictation history', '', toggle('history.enabled', true))
        + row('Retention', '', num('history.retention_days', 90, 'days'))
        + row('Redact secrets', 'Mask anything that looks like a key or password in history.', toggle('history.redact_secrets', true))
        + '<p class="privacy-note">Audio never leaves this machine unless you pick a cloud provider.</p>';
    },
  },
  {
    id: 'apikeys', title: 'API Keys',
    summary() {
      if (apiKeysErr) return 'unavailable';
      if (!apiKeys) return 'loading\u2026';
      const active = apiKeys.filter((k) => !k.revoked).length;
      return active ? active + (active === 1 ? ' key' : ' keys') : 'none yet';
    },
    html() { return apiKeysHtml(); },
  },
  {
    id: 'speakers', title: 'Speakers (voice ID)',
    summary() {
      if (!gv('speaker.enabled', false)) return 'off';
      if (speakersErr) return 'unavailable';
      if (!speakers) return 'loading\u2026';
      return speakers.length
        ? speakers.length + (speakers.length === 1 ? ' voice' : ' voices')
        : 'no voices yet';
    },
    html() { return speakersHtml(); },
  },
  {
    id: 'servers', title: 'Servers & Advanced',
    summary() {
      const bits = [];
      if (gv('server.wyoming.enabled', false)) bits.push('Wyoming :' + gv('server.wyoming.port', 10300));
      if (gv('server.llm.enabled', false)) bits.push('LLM :' + gv('server.llm.port', 11434));
      if (gv('server.web.enabled', false)) bits.push('Web :' + gv('server.web.port', 10808));
      if (gv('mcp.enabled', true)) bits.push('MCP on');
      return bits.join(' \u00b7 ') || 'all off';
    },
    html() {
      return srvCard('Wyoming server (STT/TTS over the LAN)',
        srvField('Bind', srvInput('server.wyoming.bind', '127.0.0.1'))
        + srvField('Port', srvNum('server.wyoming.port', 10300))
        + srvField('Token ref', srvInput('server.wyoming.auth_token_ref', '', 'none')),
        'server.wyoming.enabled')
        + srvCard('API server (OpenAI + Ollama compatible)',
          srvField('Bind', srvInput('server.llm.bind', '127.0.0.1'))
          + srvField('Port', srvNum('server.llm.port', 11434))
          + srvField('Require API key', toggle('server.llm.auth', true))
          + srvField('Model override', srvInput('server.llm.model', '', '\u2014')),
          'server.llm.enabled')
        + srvCard('Web settings (this page \u2014 changes apply after restart)',
          srvField('Bind', srvInput('server.web.bind', '127.0.0.1'))
          + srvField('Port', srvNum('server.web.port', 10808))
          + srvField('Require API key', toggle('server.web.auth', true)),
          'server.web.enabled')
        + row('Network name', 'How this machine appears to other Fono instances. Empty = fono-<hostname>.',
          txt('network.instance_name', { w: 180, ph: 'auto' }))
        + row('Agent integration (MCP)', 'Let coding agents drive dictation and ask for audio (stdio only).',
          toggle('mcp.enabled', true))
        + row('Text injection backend', 'auto picks the best keystroke path for your session.',
          sel('inject.backend', [['auto', 'auto'], ['clipboard', 'clipboard'], ['xdotool', 'xdotool'], ['wtype', 'wtype'], ['ydotool', 'ydotool'], ['xtest', 'xtest'], ['enigo', 'enigo']], 'auto'))
        + row('Check for updates', 'One check on daemon start; nothing periodic.',
          sel('update.channel', [['stable', 'stable'], ['prerelease', 'prerelease']], 'stable') + toggle('update.auto_check', true))
        + '<details class="prompt-d"><summary><span class="lbl">Advanced tuning</span><span class="hint">rarely needed</span><span style="margin-left:auto" class="hint">show \u25be</span></summary>'
        + row('Voice activity detection', 'energy = built-in RMS gate; off disables silence handling.',
          sel('audio.vad_backend', [['energy', 'energy'], ['off', 'off']], 'energy'))
        + row('Cloud language-mismatch rerun', 'Retry cloud STT with a cached language when detection is off-list.',
          toggle('general.cloud_rerun_on_language_mismatch', true))
        + row('Whisper threads', '0 = auto-detect physical cores.', num('stt.local.threads', 0))
        + row('Skip cleanup below', 'Transcripts shorter than this many words skip the LLM.', num('polish.skip_if_words_lt', 3, 'words'))
        + row('Stream local cleanup into typing', 'Type the local model\u2019s output word by word.', toggle('polish.stream_injection', true))
        + row('Wake refractory window', 'Ignore re-fires after a detection.', num('wakeword.refractory_ms', 800, 'ms'))
        + row('Overlay VU bar', 'advanced overlays the auto-stop debug signals.',
          sel('overlay.volume_bar', [['off', 'off'], ['simple', 'simple'], ['advanced', 'advanced']], 'off'))
        + '<div class="subhead">Live transcript pipeline</div>'
        + row('Initial chunk window', '', num('interactive.chunk_ms_initial', 600, 'ms'))
        + row('Steady chunk window', '', num('interactive.chunk_ms_steady', 1500, 'ms'))
        + row('Cleanup on finalize', '', toggle('interactive.cleanup_on_finalize', true))
        + row('Cloud preview interval', '&gt;3.0 disables the preview lane (free-tier safe).', flt('interactive.streaming_interval', 1.0, 'seconds'))
        + row('Hold-release grace', '', num('interactive.hold_release_grace_ms', 150, 'ms'))
        + '<div class="subhead">Agent integration (MCP)</div>'
        + row('Mirror speech to stdout', '', toggle('mcp.mirror_to_stdout', false))
        + row('Listen ceiling', '', num('mcp.listen_max_seconds', 45, 'seconds'))
        + row('Confirm timeout', '', num('mcp.confirm_timeout_seconds', 10, 'seconds'))
        + row('Relevance filter', 'Discard transcripts that don\u2019t answer the agent\u2019s question.',
          sel('mcp.relevance_filter', [['off', 'off'], ['heuristic', 'heuristic'], ['llm', 'llm']], 'heuristic'))
        + row('Max rejections', '', num('mcp.relevance_max_rejections', 2))
        + row('Voice gender preference', '', sel('mcp.voice_gender', [['', 'any'], ['female', 'female'], ['male', 'male']], ''))
        + row('Auto-assign voices', 'Give each program a stable palette voice.', toggle('mcp.auto_assign_voices', true))
        + '</details>';
    },
  },
];
function keycapRow(lbl, desc, path) { return row(lbl, desc, keycap(path)); }

// ---------- inbound API keys (async: GET/POST/PATCH/DELETE /api/apikeys) ----------
// These guard the local LLM/STT/TTS API and this settings page when
// authentication is on. Loaded lazily after config; the section
// re-renders itself on load and after every mutation. `newKeySecret`
// holds the just-created plaintext secret so it can be shown exactly
// once — it is never persisted client-side beyond the reveal.
let apiKeys = null, apiKeysErr = null, newKeySecret = null;
function refreshApiKeysSection() {
  const sec = FONO_SECTIONS.find((s) => s.id === 'apikeys');
  if (sec && document.getElementById('d-apikeys')) renderSection(sec);
}
async function loadApiKeys() {
  try {
    const r = await api('/api/apikeys');
    apiKeys = (r && r.keys) || [];
    apiKeysErr = null;
  } catch (err) {
    apiKeys = null;
    apiKeysErr = err.message;
  }
  refreshApiKeysSection();
}
function fmtDate(ts) { return ts ? new Date(ts * 1000).toLocaleDateString() : '\u2014'; }
function keyExpiryCell(k) {
  if (!k.expires_at) return '<span class="hint">Never</span>';
  const soon = k.expires_at * 1000 < Date.now() + 7 * 864e5;
  return '<span' + (soon ? ' class="key-exp-warn"' : '') + '>' + fmtDate(k.expires_at) + '</span>';
}
function apiKeysHtml() {
  let out = '<p class="hint">These keys authenticate callers to the local LLM, speech-to-text and '
    + 'text-to-speech API, and to this settings page, whenever authentication is on '
    + '(see Servers &amp; Advanced). The secret is shown once at creation and stored only as a '
    + 'hash \u2014 it can never be shown again.</p>';
  if (newKeySecret) {
    out += '<div class="key-reveal"><div class="lbl">New key \u2014 copied to your clipboard. It won\u2019t be shown again.</div>'
      + '<div class="key-reveal-row"><code class="mono">' + esc(newKeySecret) + '</code></div></div>';
  }
  out += '<div class="key-new-row">'
    + '<input class="input" id="newkeyname" placeholder="Key name, e.g. laptop" style="width:200px" autocomplete="off" />'
    + '<select class="select" id="newkeyexpiry" title="When this key stops working">'
    + '<option value="0">No expiry</option>'
    + '<option value="7">Expires in 7 days</option>'
    + '<option value="30">Expires in 30 days</option>'
    + '<option value="90">Expires in 90 days</option>'
    + '<option value="365">Expires in 1 year</option>'
    + '</select>'
    + '<button class="btn primary" type="button" data-key-new>Create API Key</button></div>';
  if (apiKeysErr) return out + '<p class="privacy-note">Could not load keys: ' + esc(apiKeysErr) + '</p>';
  if (!apiKeys) return out + '<p class="hint">Loading\u2026</p>';
  if (!apiKeys.length) return out + '<p class="hint">No API keys yet.</p>';
  const rows = apiKeys.map((k) =>
    '<tr' + (k.revoked ? ' class="key-revoked"' : '') + '>'
    + '<td>' + esc(k.name) + (k.revoked ? ' <span class="hint">(revoked)</span>' : '') + '</td>'
    + '<td class="mono">' + esc(k.masked) + '</td>'
    + '<td>' + fmtDate(k.created_at) + '</td>'
    + '<td>' + (k.last_used_at ? fmtDate(k.last_used_at) : '<span class="hint">Never</span>') + '</td>'
    + '<td>' + keyExpiryCell(k) + '</td>'
    + '<td>' + (k.usage_month || 0) + '</td>'
    + '<td class="key-actions">'
    + keyIconBtn('key-rename', k.id, '\u270E', 'Rename')
    + (k.revoked
      ? keyIconBtn('key-restore', k.id, '\u21BA', 'Restore')
      : keyIconBtn('key-revoke', k.id, '\u2298', 'Revoke'))
    + keyIconBtn('key-delete', k.id, '\u2715', 'Delete', 'danger')
    + '</td></tr>').join('');
  return out + '<table class="key-table"><thead><tr>'
    + '<th>Name</th><th>Secret</th><th>Created</th><th>Last used</th><th>Expires</th>'
    + '<th>Usage (month)</th><th></th></tr></thead><tbody>' + rows + '</tbody></table>';
}
// Compact icon action button for a key row. `action` is the data-* name
// (e.g. 'key-rename'); the glyph is a system-font character so we stay
// image-/icon-font-free. `title`/`aria-label` carry the accessible name.
function keyIconBtn(action, id, glyph, label, extra) {
  return '<button class="keybtn' + (extra ? ' ' + extra : '') + '" type="button" data-'
    + action + '="' + id + '" title="' + label + '" aria-label="' + label + '">' + glyph + '</button>';
}
async function createApiKey() {
  const inp = document.getElementById('newkeyname');
  const name = inp && inp.value.trim();
  if (!name) { toast('Enter a key name first', true); return; }
  const sel = document.getElementById('newkeyexpiry');
  const days = sel ? parseInt(sel.value, 10) : 0;
  const body = { name };
  if (days > 0) body.expires_at = Math.floor(Date.now() / 1000) + days * 86400;
  try {
    const r = await api('/api/apikeys', { method: 'POST', body: JSON.stringify(body) });
    newKeySecret = r.secret;
    if (newKeySecret && navigator.clipboard) {
      navigator.clipboard.writeText(newKeySecret).then(
        () => toast('Key created \u2014 copied to clipboard'),
        () => toast('Key created \u2014 copy it manually, clipboard blocked', true),
      );
    } else {
      toast('Key created \u2014 copy it now, it won\u2019t be shown again', true);
    }
    await loadApiKeys();
  } catch (err) { toast('Could not create key: ' + err.message, true); }
}
async function renameApiKey(id) {
  const cur = (apiKeys.find((k) => k.id === id) || {}).name || '';
  const name = prompt('New name for this key:', cur);
  if (name == null || !name.trim()) return;
  try {
    await api('/api/apikeys/' + id, { method: 'PATCH', body: JSON.stringify({ name: name.trim() }) });
    await loadApiKeys();
  } catch (err) { toast('Could not rename: ' + err.message, true); }
}
async function setApiKeyRevoked(id, revoked) {
  try {
    await api('/api/apikeys/' + id, { method: 'PATCH', body: JSON.stringify({ revoked }) });
    toast(revoked ? 'Key revoked' : 'Key restored');
    await loadApiKeys();
  } catch (err) { toast('Could not update key: ' + err.message, true); }
}
async function deleteApiKey(id) {
  if (!confirm('Permanently delete this key and its usage history? This cannot be undone.')) return;
  try {
    await api('/api/apikeys/' + id, { method: 'DELETE' });
    toast('Key deleted');
    await loadApiKeys();
  } catch (err) { toast('Could not delete key: ' + err.message, true); }
}


// ---------- enrolled speakers (async: GET/PATCH/DELETE /api/speakers) ----------
// Local voice identification. Metadata only crosses the wire — voice-print
// embeddings never leave the daemon. Enrollment and "test my voice"
// calibration arrive with the hosted model pack; until then this manages
// the enable/threshold settings and lists/renames/removes whatever the CLI
// (`fono speaker enroll`) has captured.
let speakers = null, speakersErr = null;
// Enrollment UI state, preserved across section re-renders.
let spkEnrollName = '';
let spkRec = null;
// Captured-but-not-yet-submitted 16 kHz PCM, awaiting Submit or Discard.
let spkPending = null;
// "Test my voice" calibration state, preserved across section re-renders.
// `spkCalClips` accumulates held-out 16 kHz PCM clips; `spkCalResult` holds
// the last calibrate response so its histogram survives a re-render.
let spkCalSpeakerId = null, spkCalClips = [], spkCalRec = null, spkCalResult = null, spkCalBusy = false;
function refreshSpeakersSection() {
  const sec = FONO_SECTIONS.find((s) => s.id === 'speakers');
  if (sec && document.getElementById('d-speakers')) renderSection(sec);
}
async function loadSpeakers() {
  try {
    const r = await api('/api/speakers');
    speakers = (r && r.speakers) || [];
    speakersErr = null;
  } catch (err) {
    speakers = null;
    speakersErr = err.message;
  }
  refreshSpeakersSection();
}
function speakersHtml() {
  let out = row('Identify who is speaking', 'Tag transcripts with a speaker name using a local voice model. '
    + 'This is identification and a convenience gate \u2014 not authentication.', toggle('speaker.enabled', false, 'speakers'))
    + row('Model', 'Local speaker-embedding model.',
      sel('speaker.model', [['redimnet2-b3', 'ReDimNet2-B3 (recommended)'], ['redimnet2-b6', 'ReDimNet2-B6 (max accuracy)']], 'redimnet2-b3'))
    + row('Decision threshold', 'auto tunes from your calibration; or pin a fixed 0\u20131 score.',
      txt('speaker.threshold', { w: 120, ph: 'auto' }))
    + row('Minimum speech', 'Seconds of speech gathered before a decision.', flt('speaker.min_speech_secs', 3.0, 'seconds'));
  out += '<div class="enroll-card">'
    + '<div class="enroll-row">'
    + '<input id="spk-enroll-name" class="input enroll-name" type="text" placeholder="Name (e.g. Alice)" value="' + esc(spkEnrollName) + '" autocomplete="off" />'
    + '<select id="spk-enroll-device" class="select enroll-device"><option value="">Default microphone</option></select>'
    + '<button class="btn" id="spk-record-btn" data-spk-record type="button">Record</button>'
    + '<button class="btn spk-hidden" id="spk-submit-btn" data-spk-submit type="button">Submit</button>'
    + '<button class="btn danger spk-hidden" id="spk-discard-btn" data-spk-discard type="button">Discard</button>'
    + '</div>'
    + '<div id="spk-meter" class="spk-meter spk-hidden"><div id="spk-meter-bar" class="spk-meter-bar"></div></div>'
    + '<p id="spk-enroll-status" class="hint">Records locally in your browser, resamples to 16&nbsp;kHz, and stores only the derived voice print &mdash; the audio never leaves your machine. Record a few seconds, then submit or discard; repeat 2&ndash;3 times per person for a solid profile.</p>'
    + '</div>';
  if (speakersErr) return out + '<p class="privacy-note">Could not load speakers: ' + esc(speakersErr) + '</p>';
  if (!speakers) return out + '<p class="hint">Loading\u2026</p>';
  if (!speakers.length) return out + '<p class="hint">No enrolled voices yet.</p>';
  const rows = speakers.map((s) => {
    const st = spkStrength(s);
    return '<tr>'
    + '<td>' + esc(s.name) + '</td>'
    + '<td>' + (s.utterance_count || 0) + '</td>'
    + '<td><span class="spk-strength ' + st.cls + '" title="' + esc(st.nudge) + '">' + st.label + '</span></td>'
    + '<td>' + (s.calibrated ? '<span>\u2713</span>' : '<span class="hint">\u2014</span>') + '</td>'
    + '<td>' + fmtDate(s.updated_at) + '</td>'
    + '<td class="key-actions">'
    + keyIconBtn('spk-rename', s.id, '\u270E', 'Rename')
    + keyIconBtn('spk-delete', s.id, '\u2715', 'Delete', 'danger')
    + '</td></tr>';
  }).join('');
  return out + '<table class="key-table"><thead><tr>'
    + '<th>Name</th><th>Utterances</th><th>Strength</th><th>Calibrated</th><th>Updated</th><th></th>'
    + '</tr></thead><tbody>' + rows + '</tbody></table>'
    + calibrateCardHtml();
}

// ---------- "test my voice" calibration card ----------
// Records a few *held-out* clips (separate from enrollment) and POSTs them to
// /api/speakers/{id}/calibrate, which scores them against the chosen voice and
// a large impostor cohort. The response drives a genuine-vs-impostor histogram,
// an equal-error-rate readout, a plain-language verdict, and a one-click
// "use recommended threshold" that writes speaker.threshold. No audio is stored.
function calibrateCardHtml() {
  if (!speakers || !speakers.length) return '';
  if (spkCalSpeakerId == null || !speakers.some((s) => s.id === spkCalSpeakerId)) {
    spkCalSpeakerId = speakers[0].id;
  }
  const opts = speakers.map((s) =>
    '<option value="' + s.id + '"' + (s.id === spkCalSpeakerId ? ' selected' : '') + '>'
    + esc(s.name) + '</option>').join('');
  const n = spkCalClips.length;
  const canRun = n >= 2 && !spkCalBusy;
  return '<div class="enroll-card cal-card">'
    + '<div class="cal-title">Test my voice</div>'
    + '<p class="hint">Record 3\u20135 short <em>new</em> clips of the chosen voice (don\u2019t reuse enrollment audio). '
    + 'Fono measures how well it tells this voice apart from others, on your mic and room, and can set the decision threshold for you.</p>'
    + '<div class="enroll-row">'
    + '<select id="spk-cal-speaker" class="select enroll-name">' + opts + '</select>'
    + '<select id="spk-cal-device" class="select enroll-device"><option value="">Default microphone</option></select>'
    + '<button class="btn" id="spk-cal-record" data-spk-cal-record type="button">Record clip</button>'
    + '<button class="btn primary" data-spk-cal-run type="button"' + (canRun ? '' : ' disabled') + '>Run test</button>'
    + '<button class="btn" data-spk-cal-clear type="button"' + (n && !spkCalBusy ? '' : ' disabled') + '>Clear</button>'
    + '</div>'
    + '<div id="spk-cal-meter" class="spk-meter spk-hidden"><div id="spk-cal-meter-bar" class="spk-meter-bar"></div></div>'
    + '<p id="spk-cal-status" class="hint">' + (n ? (n + (n === 1 ? ' clip' : ' clips') + ' captured' + (n < 2 ? ' \u2014 record at least one more, then Run test.' : ' \u2014 Run test when ready.')) : 'No test clips yet.') + '</p>'
    + '<div id="spk-cal-results">' + calResultsHtml() + '</div>'
    + '</div>';
}

// Heuristic profile-strength bucket. The count/seconds/device signals are
// only *proxies* (five clipped or silent clips can still look "strong"); the
// authoritative quality measure is the voice test (Step 3), so the nudge
// pushes toward stronger enrollment until calibration exists.
function spkStrength(s) {
  const n = s.utterance_count || 0;
  const secs = s.total_secs || 0;
  const devs = s.source_count || 0;
  if (n === 0) return { label: 'empty', cls: 'weak', nudge: 'Record a first sample.' };
  const factors = [
    { ok: n >= 4, msg: 'Add more samples (aim for 4\u20135).' },
    { ok: secs >= 15, msg: 'Record more speech (aim ~15\u201330\u00a0s total).' },
    { ok: devs >= 2, msg: 'Enroll on another microphone you use.' },
  ];
  const failed = factors.filter((f) => !f.ok);
  const label = failed.length === 0 ? 'strong' : failed.length === 1 ? 'ok' : 'weak';
  const nudge = failed.length ? failed[0].msg : 'Solid profile.';
  return { label, cls: label, nudge };
}

// Render the last calibrate result: histogram + EER + verdict + apply button.
function calResultsHtml() {
  const r = spkCalResult;
  if (!r) return '';
  const eerPct = (r.eer * 100).toFixed(1);
  const v = calVerdict(r.eer);
  const lat = r.latency_ms && r.latency_ms.count
    ? ' \u00b7 \u2248' + Math.round(r.latency_ms.mean) + '\u00a0ms/check on this machine' : '';
  const g = (r.genuine && r.genuine.scores) || [];
  const im = (r.impostor && r.impostor.scores) || [];
  const thr = typeof r.eer_threshold === 'number' ? r.eer_threshold : null;
  return '<div class="cal-results">'
    + calHistogramSvg(g, im, thr)
    + '<div class="cal-verdict"><span class="spk-strength ' + v.cls + '">' + v.label + '</span> '
    + '<strong>' + eerPct + '%</strong> equal-error rate' + lat + '</div>'
    + '<p class="hint">' + v.msg + '</p>'
    + '<div class="cal-legend"><span class="cal-swatch cal-genuine"></span>you ('
    + (r.genuine ? r.genuine.trials : 0) + ') <span class="cal-swatch cal-impostor"></span>others ('
    + (r.impostor ? r.impostor.trials : 0) + ')'
    + (thr != null ? ' <span class="cal-swatch cal-thr"></span>threshold ' + thr.toFixed(3) : '') + '</div>'
    + (thr != null
      ? '<button class="btn" data-spk-cal-apply="' + thr.toFixed(4) + '" type="button">Use recommended threshold ('
        + thr.toFixed(2) + ')</button>'
      : '')
    + '</div>';
}
// Plain-language verdict bucketed by EER.
function calVerdict(eer) {
  if (eer <= 0.01) return { label: 'excellent', cls: 'strong', msg: 'Your voice is easy to tell apart here.' };
  if (eer <= 0.05) return { label: 'good', cls: 'strong', msg: 'Reliable separation on this mic and room.' };
  if (eer <= 0.10) return { label: 'fair', cls: 'ok', msg: 'Usable, but more or cleaner samples would help.' };
  return { label: 'weak', cls: 'weak', msg: 'Hard to separate \u2014 enroll more clips in your real environment.' };
}
// Inline-SVG overlaid histogram of the genuine vs impostor score distributions,
// with a vertical marker at the recommended threshold. No chart library.
function calHistogramSvg(genuine, impostor, thr) {
  const all = genuine.concat(impostor);
  if (!all.length) return '';
  let lo = Math.min.apply(null, all), hi = Math.max.apply(null, all);
  if (thr != null) { lo = Math.min(lo, thr); hi = Math.max(hi, thr); }
  if (hi - lo < 1e-6) { lo -= 0.5; hi += 0.5; }
  const pad = (hi - lo) * 0.05; lo -= pad; hi += pad;
  const W = 320, H = 96, BINS = 24;
  const bin = (hi - lo) / BINS;
  const gh = new Array(BINS).fill(0), ih = new Array(BINS).fill(0);
  const fill = (arr, dst) => arr.forEach((x) => {
    let k = Math.floor((x - lo) / bin); if (k < 0) k = 0; if (k >= BINS) k = BINS - 1; dst[k]++;
  });
  fill(genuine, gh); fill(impostor, ih);
  const peak = Math.max(1, Math.max.apply(null, gh.concat(ih)));
  const bw = W / BINS;
  const bars = (arr, cls) => arr.map((c, k) => {
    if (!c) return '';
    const h = c / peak * (H - 12);
    return '<rect class="' + cls + '" x="' + (k * bw).toFixed(1) + '" y="' + (H - h).toFixed(1)
      + '" width="' + (bw - 1).toFixed(1) + '" height="' + h.toFixed(1) + '"/>';
  }).join('');
  let marker = '';
  if (thr != null) {
    const x = ((thr - lo) / (hi - lo) * W).toFixed(1);
    marker = '<line class="cal-thr-line" x1="' + x + '" y1="0" x2="' + x + '" y2="' + H + '"/>';
  }
  return '<svg class="cal-hist" viewBox="0 0 ' + W + ' ' + H + '" preserveAspectRatio="none" role="img" '
    + 'aria-label="Score distribution of your voice versus others">'
    + bars(ih, 'cal-impostor') + bars(gh, 'cal-genuine') + marker + '</svg>'
    + '<div class="cal-axis"><span>' + lo.toFixed(2) + '</span><span>score</span><span>' + hi.toFixed(2) + '</span></div>';
}
async function renameSpeaker(id) {
  const cur = (speakers.find((s) => s.id === id) || {}).name || '';
  const name = prompt('New name for this voice:', cur);
  if (name == null || !name.trim()) return;
  try {
    await api('/api/speakers/' + id, { method: 'PATCH', body: JSON.stringify({ name: name.trim() }) });
    await loadSpeakers();
  } catch (err) { toast('Could not rename: ' + err.message, true); }
}
async function deleteSpeaker(id) {
  if (!confirm('Permanently delete this voice and all its voice prints? This cannot be undone.')) return;
  try {
    await api('/api/speakers/' + id, { method: 'DELETE' });
    toast('Voice deleted');
    await loadSpeakers();
  } catch (err) { toast('Could not delete voice: ' + err.message, true); }
}

// ---------- speaker enrollment (browser capture) ----------
// Records mic audio with the browser's DSP disabled (no AGC/NS/AEC so the
// voice print matches raw dictation audio), resamples to 16 kHz mono, and
// POSTs 16-bit PCM. Only the derived embedding is stored server-side.
async function spkPopulateDevices(selectedId, elId) {
  const devEl = document.getElementById(elId || 'spk-enroll-device');
  if (!devEl || !navigator.mediaDevices || !navigator.mediaDevices.enumerateDevices) return;
  try {
    const devs = await navigator.mediaDevices.enumerateDevices();
    const mics = devs.filter((d) => d.kind === 'audioinput');
    devEl.innerHTML = '<option value="">Default microphone</option>' + mics.map((d) =>
      '<option value="' + esc(d.deviceId) + '"' + (d.deviceId === selectedId ? ' selected' : '') + '>'
      + esc(d.label || 'Microphone') + '</option>').join('');
  } catch (_e) { /* labels need permission; ignore */ }
}
function spkStatus(msg) {
  const el = document.getElementById('spk-enroll-status');
  if (el) el.textContent = msg;
}
// Toggle the enroll buttons between the idle/recording/review phases.
function spkSetPhase(phase) {
  const rec = document.getElementById('spk-record-btn');
  const sub = document.getElementById('spk-submit-btn');
  const dis = document.getElementById('spk-discard-btn');
  if (!rec || !sub || !dis) return;
  if (phase === 'recording') {
    rec.textContent = 'Stop';
    rec.classList.add('danger');
    rec.classList.remove('spk-hidden');
    sub.classList.add('spk-hidden');
    dis.classList.add('spk-hidden');
  } else if (phase === 'review') {
    rec.classList.add('spk-hidden');
    rec.classList.remove('danger');
    sub.classList.remove('spk-hidden');
    dis.classList.remove('spk-hidden');
  } else { // idle
    rec.textContent = 'Record';
    rec.classList.remove('danger', 'spk-hidden');
    sub.classList.add('spk-hidden');
    dis.classList.add('spk-hidden');
  }
}
// Start recording, or stop-and-hold for review if already recording.
async function spkRecordToggle() {
  if (spkRec) { await spkStopToReview(); return; }
  const nameEl = document.getElementById('spk-enroll-name');
  const name = (nameEl && nameEl.value.trim()) || '';
  if (!name) { toast('Enter a name first', true); if (nameEl) nameEl.focus(); return; }
  const devEl = document.getElementById('spk-enroll-device');
  const deviceId = devEl && devEl.value ? devEl.value : null;
  try {
    const audio = { echoCancellation: false, noiseSuppression: false, autoGainControl: false, channelCount: 1 };
    if (deviceId) audio.deviceId = { exact: deviceId };
    const stream = await navigator.mediaDevices.getUserMedia({ audio });
    await spkPopulateDevices(deviceId);
    const Ctx = window.AudioContext || window.webkitAudioContext;
    const ctx = new Ctx();
    const source = ctx.createMediaStreamSource(stream);
    const proc = ctx.createScriptProcessor(4096, 1, 1);
    const chunks = [];
    proc.onaudioprocess = (ev) => {
      const ch = ev.inputBuffer.getChannelData(0);
      chunks.push(new Float32Array(ch));
      let sq = 0, pk = 0;
      for (let i = 0; i < ch.length; i++) { const a = Math.abs(ch[i]); if (a > pk) pk = a; sq += ch[i] * ch[i]; }
      spkUpdateMeter(Math.sqrt(sq / Math.max(1, ch.length)), pk);
    };
    source.connect(proc); proc.connect(ctx.destination);
    spkRec = { stream, ctx, source, proc, chunks, sampleRate: ctx.sampleRate };
    spkMeterShow(true);
    spkSetPhase('recording');
    spkStatus('Recording\u2026 speak naturally for a few seconds, then press Stop.');
  } catch (err) { toast('Microphone error: ' + err.message, true); }
}
// Stop the mic, resample, and hold the clip for the user to submit or discard.
async function spkStopToReview() {
  const rec = spkRec; spkRec = null;
  try {
    rec.proc.disconnect(); rec.source.disconnect();
    rec.stream.getTracks().forEach((t) => t.stop());
    await rec.ctx.close();
  } catch (_e) { /* teardown best-effort */ }
  let total = 0; rec.chunks.forEach((c) => { total += c.length; });
  const merged = new Float32Array(total);
  let off = 0; rec.chunks.forEach((c) => { merged.set(c, off); off += c.length; });
  spkStatus('Processing\u2026');
  const pcm = await spkResampleTo16k(merged, rec.sampleRate);
  if (pcm.length < 16000) {
    spkPending = null;
    spkSetPhase('idle');
    spkStatus('That was too short \u2014 record at least about a second of speech.');
    return;
  }
  const metrics = spkAnalyze(pcm);
  spkPending = { pcm, metrics };
  spkMeterShow(false);
  spkSetPhase('review');
  const warns = spkQualityWarnings(metrics);
  const secs = metrics.duration_secs.toFixed(1);
  if (warns.length) {
    spkStatus('Captured ' + secs + '\u00a0s, but ' + warns.join('; ') + '. Submit anyway, or discard and re-record.');
  } else {
    spkStatus('Captured ' + secs + '\u00a0s \u2014 audio looks good. Submit to enroll, or discard and try again.');
  }
}
// Send the held clip to the server as a new enrollment sample.
async function spkSubmit() {
  if (!spkPending) return;
  const nameEl = document.getElementById('spk-enroll-name');
  const name = (nameEl && nameEl.value.trim()) || spkEnrollName;
  if (!name) { toast('Enter a name first', true); if (nameEl) nameEl.focus(); return; }
  const pcm = spkPending.pcm;
  const m = spkPending.metrics;
  spkStatus('Enrolling\u2026');
  try {
    const resp = await api('/api/speakers', { method: 'POST', body: JSON.stringify({
      name, audio_pcm16: spkFloatToB64(pcm), sample_rate: 16000, capture_source: 'browser',
      duration_secs: m.duration_secs, loudness_dbfs: m.loudness_dbfs, snr_db: m.snr_db,
    }) });
    spkPending = null;
    spkSetPhase('idle');
    spkStatus('');
    const sm = resp && typeof resp.self_match === 'number' ? resp.self_match : null;
    if (sm == null) {
      toast('Enrolled the first voice sample for ' + name);
    } else if (sm >= 0.4) {
      toast('Enrolled \u2014 \u2713 this sample matches ' + name + '\u2019s profile');
    } else {
      toast('Enrolled, but this sample sounds different from ' + name + '\u2019s other samples \u2014 check the mic', true);
    }
    await loadSpeakers();
  } catch (err) { toast('Enrollment failed: ' + err.message, true); }
}
// Throw the held clip away and return to the idle state.
function spkDiscard() {
  spkPending = null;
  spkMeterShow(false);
  spkSetPhase('idle');
  spkStatus('Discarded. Record again when ready.');
}
// Show/hide and reset the live input meter.
function spkMeterShow(on, meterId) {
  meterId = meterId || 'spk-meter';
  const meter = document.getElementById(meterId);
  const bar = document.getElementById(meterId + '-bar');
  if (!meter) return;
  meter.classList.toggle('spk-hidden', !on);
  if (bar && !on) { bar.style.width = '0%'; bar.classList.remove('clip'); }
}
// Map the running RMS/peak to the meter bar (\u221260..0 dBFS \u2192 0..100%).
function spkUpdateMeter(rms, peak, barId) {
  const bar = document.getElementById(barId || 'spk-meter-bar');
  if (!bar) return;
  const db = 20 * Math.log10(rms + 1e-9);
  const pct = Math.max(0, Math.min(100, (db + 60) / 60 * 100));
  bar.style.width = pct.toFixed(0) + '%';
  bar.classList.toggle('clip', peak >= 0.99);
}
// Intrinsic capture-quality metrics, computed once on the resampled 16 kHz clip.
// These are recompute-impossible after the audio is dropped, so they ride the
// enroll POST and are persisted per utterance.
function spkAnalyze(pcm) {
  const n = pcm.length;
  let sumSq = 0, peak = 0;
  for (let i = 0; i < n; i++) { const a = Math.abs(pcm[i]); if (a > peak) peak = a; sumSq += pcm[i] * pcm[i]; }
  const rms = Math.sqrt(sumSq / Math.max(1, n));
  const loudness = 20 * Math.log10(rms + 1e-9);
  // Per-frame (25 ms) energies \u2192 SNR from the 10th vs 90th percentile.
  const F = 400; const energies = [];
  for (let i = 0; i + F <= n; i += F) { let e = 0; for (let j = 0; j < F; j++) { const s = pcm[i + j]; e += s * s; } energies.push(e / F); }
  let snr = null;
  if (energies.length >= 4) {
    const sorted = energies.slice().sort((a, b) => a - b);
    const noise = sorted[Math.floor(sorted.length * 0.1)];
    const speech = sorted[Math.floor(sorted.length * 0.9)];
    snr = 10 * Math.log10((speech + 1e-12) / (noise + 1e-12));
  }
  return {
    duration_secs: +(n / 16000).toFixed(2),
    loudness_dbfs: +loudness.toFixed(1),
    snr_db: snr == null ? null : +snr.toFixed(1),
    peak: +peak.toFixed(3),
  };
}
// Plain-language warnings from the intrinsic metrics; empty means the clip is clean.
function spkQualityWarnings(m) {
  const w = [];
  if (m.peak >= 0.99) w.push('the audio is clipping (move back or lower input gain)');
  if (m.loudness_dbfs < -45) w.push('it is very quiet (move closer or raise input gain)');
  if (m.snr_db != null && m.snr_db < 10) w.push('the background sounds noisy');
  return w;
}
async function spkResampleTo16k(samples, srcRate) {
  if (srcRate === 16000 || !samples.length) return samples;
  const len = Math.max(1, Math.round(samples.length * 16000 / srcRate));
  const Off = window.OfflineAudioContext || window.webkitOfflineAudioContext;
  const off = new Off(1, len, 16000);
  const buf = off.createBuffer(1, samples.length, srcRate);
  buf.copyToChannel(samples, 0);
  const src = off.createBufferSource(); src.buffer = buf; src.connect(off.destination); src.start();
  const rendered = await off.startRendering();
  return rendered.getChannelData(0);
}
function spkFloatToB64(f32) {
  const bytes = new Uint8Array(f32.length * 2);
  const view = new DataView(bytes.buffer);
  for (let i = 0; i < f32.length; i++) {
    const s = Math.max(-1, Math.min(1, f32[i]));
    view.setInt16(i * 2, s < 0 ? s * 0x8000 : s * 0x7fff, true);
  }
  let binary = '';
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

// ---------- "test my voice" calibration recorder ----------
// A self-contained recorder that appends each stop into `spkCalClips` (rather
// than the enroll submit/discard flow). Reuses the pure resample/encode/device
// helpers; the recorded audio is only used to POST /calibrate and is dropped.
function spkCalStatus(msg) {
  const el = document.getElementById('spk-cal-status');
  if (el) el.textContent = msg;
}
async function spkCalRecordToggle() {
  if (spkCalRec) { await spkCalStop(); return; }
  const devEl = document.getElementById('spk-cal-device');
  const deviceId = devEl && devEl.value ? devEl.value : null;
  try {
    const audio = { echoCancellation: false, noiseSuppression: false, autoGainControl: false, channelCount: 1 };
    if (deviceId) audio.deviceId = { exact: deviceId };
    const stream = await navigator.mediaDevices.getUserMedia({ audio });
    await spkPopulateDevices(deviceId, 'spk-cal-device');
    const Ctx = window.AudioContext || window.webkitAudioContext;
    const ctx = new Ctx();
    const source = ctx.createMediaStreamSource(stream);
    const proc = ctx.createScriptProcessor(4096, 1, 1);
    const chunks = [];
    proc.onaudioprocess = (ev) => {
      const ch = ev.inputBuffer.getChannelData(0);
      chunks.push(new Float32Array(ch));
      let sq = 0, pk = 0;
      for (let i = 0; i < ch.length; i++) { const a = Math.abs(ch[i]); if (a > pk) pk = a; sq += ch[i] * ch[i]; }
      spkUpdateMeter(Math.sqrt(sq / Math.max(1, ch.length)), pk, 'spk-cal-meter-bar');
    };
    source.connect(proc); proc.connect(ctx.destination);
    spkCalRec = { stream, ctx, source, proc, chunks, sampleRate: ctx.sampleRate };
    spkMeterShow(true, 'spk-cal-meter');
    const btn = document.getElementById('spk-cal-record');
    if (btn) { btn.textContent = 'Stop'; btn.classList.add('danger'); }
    spkCalStatus('Recording\u2026 speak a sentence, then press Stop.');
  } catch (err) { toast('Microphone error: ' + err.message, true); }
}
async function spkCalStop() {
  const rec = spkCalRec; spkCalRec = null;
  try {
    rec.proc.disconnect(); rec.source.disconnect();
    rec.stream.getTracks().forEach((t) => t.stop());
    await rec.ctx.close();
  } catch (_e) { /* teardown best-effort */ }
  spkMeterShow(false, 'spk-cal-meter');
  let total = 0; rec.chunks.forEach((c) => { total += c.length; });
  const merged = new Float32Array(total);
  let off = 0; rec.chunks.forEach((c) => { merged.set(c, off); off += c.length; });
  const pcm = await spkResampleTo16k(merged, rec.sampleRate);
  if (pcm.length < 16000) { refreshSpeakersSection(); spkCalStatus('That was too short \u2014 record about a second or more.'); return; }
  spkCalClips.push(pcm);
  refreshSpeakersSection();
}
function spkCalClear() {
  spkCalClips = [];
  spkCalResult = null;
  refreshSpeakersSection();
}
async function spkCalRun() {
  if (spkCalClips.length < 2 || spkCalBusy) return;
  const id = spkCalSpeakerId;
  spkCalBusy = true;
  refreshSpeakersSection();
  spkCalStatus('Testing your voice against a large set of other speakers\u2026');
  try {
    const clips = spkCalClips.map((pcm) => ({ audio_pcm16: spkFloatToB64(pcm), sample_rate: 16000 }));
    const resp = await api('/api/speakers/' + id + '/calibrate', { method: 'POST', body: JSON.stringify({ clips }) });
    spkCalResult = resp;
    spkCalBusy = false;
    refreshSpeakersSection();
    await loadSpeakers(); // refresh the "Calibrated" column
    toast('Voice test complete \u2014 ' + (resp.eer * 100).toFixed(1) + '% error rate');
  } catch (err) {
    spkCalBusy = false;
    refreshSpeakersSection();
    toast('Voice test failed: ' + err.message, true);
  }
}
// Write the recommended threshold into the working config; the user Saves it.
function spkCalApply(thr) {
  set(cfg, 'speaker.threshold', String(thr));
  const sec = FONO_SECTIONS.find((s) => s.id === 'speakers');
  if (sec) renderSection(sec);
  updateBar();
  toast('Threshold set to ' + thr + ' \u2014 press Save to apply');
}

// ---------- render ----------
function renderSection(s) {
  const d = document.getElementById('d-' + s.id);
  if (!d) return;
  d.querySelector('.sum').textContent = s.summary();
  d.querySelector('.body').innerHTML = s.html();
}
function renderAll() {
  const list = document.getElementById('list');
  const openState = {};
  list.querySelectorAll('details.sec').forEach((d) => { openState[d.id] = d.open; });
  list.innerHTML = FONO_SECTIONS.map((s, i) =>
    '<details class="sec" id="d-' + s.id + '"' + ((openState['d-' + s.id] !== undefined ? openState['d-' + s.id] : i === 0) ? ' open' : '') + '>'
    + '<summary><span class="chev">\u25b6</span><span class="t">' + esc(s.title) + '</span><span class="sum">' + esc(s.summary()) + '</span></summary>'
    + '<div class="body">' + s.html() + '</div></details>').join('');
  applyFilter(document.getElementById('q').value);
}
function sectionOf(el) {
  const d = el.closest('details.sec');
  return d ? FONO_SECTIONS.find((s) => 'd-' + s.id === d.id) : null;
}
function afterChange(el, rerenderSection) {
  // data-rr attributes carry the section id as a string.
  if (typeof rerenderSection === 'string') rerenderSection = FONO_SECTIONS.find((s) => s.id === rerenderSection);
  if (rerenderSection) {
    renderSection(rerenderSection);
  } else if (el) {
    const s = sectionOf(el);
    if (s) document.querySelector('#d-' + s.id + ' .sum').textContent = s.summary();
  }
  updateBar();
}
function updateBar() {
  const n = dirtyPaths().length + (vocabDirty() ? 1 : 0);
  const bar = document.getElementById('unsaved');
  bar.hidden = n === 0;
  document.getElementById('dirtymsg').textContent = n + ' unsaved change' + (n === 1 ? '' : 's');
}

// Insert data-idx into a bound path just before the final segment
// (e.g. wakeword.phrases.model + idx 1 -> wakeword.phrases.1.model).
function boundPath(el) {
  const p = el.dataset.bind;
  if (el.dataset.idx === undefined) return p;
  const parts = p.split('.');
  parts.splice(parts.length - 1, 0, el.dataset.idx);
  return parts.join('.');
}

// ---------- events ----------
document.addEventListener('change', (e) => {
  const el = e.target;
  if (el.dataset && el.dataset.vocabFrom !== undefined) {
    vocab.vocabulary[+el.dataset.vocabFrom].from =
      el.value.split(',').map((s) => s.trim()).filter(Boolean);
    afterChange(el);
    return;
  }
  if (el.dataset && el.dataset.vocabTo !== undefined) {
    vocab.vocabulary[+el.dataset.vocabTo].to = el.value.trim();
    afterChange(el);
    return;
  }
  if (!el.dataset || !el.dataset.bind) {
    // Remember which enrolled voice the calibration card targets.
    if (el.id === 'spk-cal-speaker') { spkCalSpeakerId = parseInt(el.value, 10); }
    return;
  }
  let v;
  switch (el.dataset.kind) {
    case 'toggle': v = el.checked; break;
    case 'num': v = Math.max(0, parseInt(el.value, 10) || 0); break;
    case 'float': v = parseFloat(el.value) || 0; break;
    case 'radio': if (!el.checked) return; v = el.value; break;
    default: v = el.value;
  }
  set(cfg, boundPath(el), v);
  afterChange(el, el.dataset.rr);
});

document.addEventListener('input', (e) => {
  const el = e.target;
  // Remember the voice-test sentence across section re-renders.
  if (el.classList.contains('tts-sample')) { ttsSample = el.value; return; }
  // Remember the enrollment name across section re-renders.
  if (el.id === 'spk-enroll-name') { spkEnrollName = el.value; return; }
  // Live sensitivity readout next to wake sliders.
  if (el.classList.contains('slider') && el.previousElementSibling && el.previousElementSibling.classList.contains('sens')) {
    el.previousElementSibling.textContent = Number(el.value).toFixed(2);
  }
});

document.addEventListener('click', (e) => {
  const t = e.target.closest('[data-seg],[data-pick],[data-tts-test],[data-tag-rm],[data-wake-rm],[data-wake-add],[data-vocab-rm],[data-vocab-add],[data-keycap],[data-reset],[data-key-edit],[data-key-clear],[data-key-save],[data-key-cancel],[data-key-new],[data-key-rename],[data-key-revoke],[data-key-restore],[data-key-delete],[data-spk-rename],[data-spk-delete],[data-spk-record],[data-spk-submit],[data-spk-discard],[data-spk-cal-record],[data-spk-cal-run],[data-spk-cal-clear],[data-spk-cal-apply]');
  if (!t) return;
  const secEl = t.closest('details.sec');
  const sec = secEl ? FONO_SECTIONS.find((s) => 'd-' + s.id === secEl.id) : null;

  if (t.dataset.seg) { SEG[t.dataset.seg](t.dataset.val); afterChange(t, sec); return; }
  if (t.dataset.pick) { PICK[t.dataset.pick](t.dataset.val); afterChange(t, sec); return; }
  if (t.dataset.ttsTest) {
    // Voice preview — never re-renders (that would drop the sample text
    // and stop playback); resolves the route from the live cfg.
    const wrap = t.closest('.ttstest');
    const sample = wrap && wrap.querySelector('.tts-sample');
    const status = wrap && wrap.querySelector('.tts-status');
    const text = (sample && sample.value.trim()) || 'The quick brown fox jumps over the lazy dog.';
    let model, voice;
    if (t.dataset.ttsTest === 'local') {
      model = gv('tts.local.engine', 'auto');
      voice = gv('tts.local.voice', '');
    } else {
      model = gv('tts.backend', 'openai');
      voice = gv('tts.voice', '');
    }
    playSpeech(model, voice, text, status);
    return;
  }
  if (t.dataset.tagRm !== undefined) {
    const box = t.closest('.tags');
    const arr = gv(box.dataset.tags, []).slice();
    arr.splice(parseInt(t.dataset.tagRm, 10), 1);
    set(cfg, box.dataset.tags, arr);
    afterChange(t, sec);
    return;
  }
  if (t.dataset.wakeRm !== undefined) {
    const arr = gv('wakeword.phrases', []).slice();
    arr.splice(parseInt(t.dataset.wakeRm, 10), 1);
    set(cfg, 'wakeword.phrases', arr);
    afterChange(t, sec);
    return;
  }
  if (t.dataset.wakeAdd !== undefined) {
    const arr = gv('wakeword.phrases', []).slice();
    arr.push({ model: 'hey_fono', sensitivity: 0.5, target: 'dictation' });
    set(cfg, 'wakeword.phrases', arr);
    afterChange(t, sec);
    return;
  }
  if (t.dataset.vocabRm !== undefined && vocab) {
    vocab.vocabulary.splice(parseInt(t.dataset.vocabRm, 10), 1);
    if (sec) renderSection(sec);
    updateBar();
    return;
  }
  if (t.dataset.vocabAdd !== undefined && vocab) {
    if (!Array.isArray(vocab.vocabulary)) vocab.vocabulary = [];
    vocab.vocabulary.push({ from: [], to: '' });
    if (sec) renderSection(sec);
    updateBar();
    return;
  }
  if (t.dataset.keycap) { captureKey(t); return; }
  if (t.dataset.reset) {
    const dflt = (meta && meta.defaults && meta.defaults[t.dataset.dkey]) || '';
    set(cfg, t.dataset.reset, dflt);
    afterChange(t, sec);
    return;
  }
  if (t.dataset.keyEdit) { keyEditUi(t, t.dataset.keyEdit); return; }
  if (t.dataset.keyClear) { putSecret(t.dataset.keyClear, '', sec); return; }
  if (t.dataset.keySave) {
    const input = t.parentElement.querySelector('input');
    if (input && input.value.trim()) putSecret(t.dataset.keySave, input.value.trim(), sec);
    return;
  }
  if (t.dataset.keyCancel !== undefined && sec) { renderSection(sec); }
  if (t.dataset.keyNew !== undefined) { createApiKey(); return; }
  if (t.dataset.keyRename) { renameApiKey(parseInt(t.dataset.keyRename, 10)); return; }
  if (t.dataset.keyRevoke) { setApiKeyRevoked(parseInt(t.dataset.keyRevoke, 10), true); return; }
  if (t.dataset.keyRestore) { setApiKeyRevoked(parseInt(t.dataset.keyRestore, 10), false); return; }
  if (t.dataset.keyDelete) { deleteApiKey(parseInt(t.dataset.keyDelete, 10)); return; }
  if (t.dataset.spkRename) { renameSpeaker(parseInt(t.dataset.spkRename, 10)); return; }
  if (t.dataset.spkDelete) { deleteSpeaker(parseInt(t.dataset.spkDelete, 10)); return; }
  if (t.dataset.spkRecord !== undefined) { spkRecordToggle(); return; }
  if (t.dataset.spkSubmit !== undefined) { spkSubmit(); return; }
  if (t.dataset.spkDiscard !== undefined) { spkDiscard(); return; }
  if (t.dataset.spkCalRecord !== undefined) { spkCalRecordToggle(); return; }
  if (t.dataset.spkCalRun !== undefined) { spkCalRun(); return; }
  if (t.dataset.spkCalClear !== undefined) { spkCalClear(); return; }
  if (t.dataset.spkCalApply !== undefined) { spkCalApply(t.dataset.spkCalApply); return; }

});

// Tag input: Enter or comma adds a tag.
document.addEventListener('keydown', (e) => {
  const el = e.target;
  if (el.classList && el.classList.contains('ghost') && el.closest('.tags')) {
    if (e.key === 'Enter' || e.key === ',') {
      e.preventDefault();
      const v = el.value.trim().replace(/,+$/, '');
      if (!v) return;
      const box = el.closest('.tags');
      const arr = gv(box.dataset.tags, []).slice();
      if (!arr.includes(v)) arr.push(v);
      set(cfg, box.dataset.tags, arr);
      const path = box.dataset.tags;
      const sec = sectionOf(el);
      afterChange(el, sec);
      const again = document.querySelector('.tags[data-tags="' + path + '"] .ghost');
      if (again) again.focus();
    }
    return;
  }
  if (e.key === '/' && !e.target.closest('input,textarea,select')) {
    e.preventDefault();
    document.getElementById('q').focus();
  }
});

// ---------- hotkey capture ----------
function captureKey(btn) {
  const prev = btn.textContent;
  btn.classList.add('capturing');
  btn.textContent = 'Press a key\u2026';
  const done = (e) => {
    e.preventDefault();
    e.stopPropagation();
    window.removeEventListener('keydown', done, true);
    btn.classList.remove('capturing');
    if (e.key === 'Escape' && btn.dataset.keycap !== 'hotkeys.cancel') {
      btn.textContent = prev; // Esc cancels capture (except for the cancel key itself)
      return;
    }
    const name = e.key.length === 1 ? e.key.toUpperCase() : e.key;
    btn.textContent = name;
    set(cfg, btn.dataset.keycap, name);
    afterChange(btn);
  };
  window.addEventListener('keydown', done, true);
}

// ---------- secrets (write-only) ----------
function keyEditUi(btn, env) {
  const ctl = btn.closest('.ctl');
  ctl.innerHTML = '<input class="input mono" type="password" placeholder="paste key\u2026" style="width:220px" autocomplete="off" />'
    + '<button class="btn primary" type="button" data-key-save="' + env + '">Save</button>'
    + '<button class="btn ghost" type="button" data-key-cancel>Cancel</button>'
    + '<span class="hint">saved immediately</span>';
  ctl.querySelector('input').focus();
}
async function putSecret(env, value, sec) {
  try {
    await api('/api/secret/' + env, { method: 'PUT', body: JSON.stringify({ value }) });
    if (!meta.secrets) meta.secrets = {};
    meta.secrets[env] = !!value;
    toast(value ? env + ' saved' : env + ' cleared');
  } catch (err) {
    toast('Could not save key: ' + err.message, true);
  }
  if (sec) renderSection(sec);
}

// ---------- save / discard ----------
async function saveAll() {
  try {
    let summary = '';
    if (dirtyPaths().length) {
      const res = await api('/api/config', { method: 'PUT', body: JSON.stringify(cfg) });
      orig = clone(cfg);
      summary = res.summary || 'Saved';
    }
    if (vocabDirty()) {
      const res = await api('/api/vocabulary', { method: 'PUT', body: JSON.stringify(vocab) });
      vocabOrig = clone(vocab);
      summary += (summary ? ' · ' : '') + ('vocabulary: ' + (res.summary || 'saved'));
    }
    updateBar();
    toast(summary || 'Saved');
  } catch (err) {
    toast('Save failed: ' + err.message, true);
    updateBar();
  }
}
function discardAll() {
  cfg = clone(orig);
  if (vocab != null) vocab = clone(vocabOrig);
  renderAll();
  updateBar();
}

let toastTimer = null;
function toast(msg, isErr) {
  const el = document.getElementById('toast');
  el.textContent = msg;
  el.classList.toggle('err', !!isErr);
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.hidden = true; }, isErr ? 6000 : 2500);
}

// ---------- views (hash router) ----------
// Two views share the page shell (header, toast, theme, token): the
// settings editor (default) and the doctor report. Hash routing keeps
// `?token=…` intact across navigation — a real path would drop it.
function currentView() { return location.hash === '#/doctor' ? 'doctor' : 'settings'; }
function showView() {
  const v = currentView();
  document.getElementById('view-settings').hidden = v !== 'settings';
  document.getElementById('view-doctor').hidden = v !== 'doctor';
  document.getElementById('verchip').textContent =
    v + (meta && meta.version ? ' \u00b7 v' + meta.version : '');
  if (v === 'doctor') renderDoctor();
}
window.addEventListener('hashchange', showView);

// ---------- doctor ----------
// Structured report from GET /api/doctor: { aggregate, generated_at,
// version, variant, sections: [{ title, checks: [{label, detail,
// severity}] }] }. Severity is ok|warn|fail|info. Fetched once on page
// load (drives the header icon) and again on explicit re-run — never
// polled, the daemon stays quiet.
let doctor = null, doctorErr = null, doctorBusy = false;
const SEV_GLYPH = { ok: '\u2713', warn: '\u26a0', fail: '\u2715', busy: '\u2026' };
const SEV_TITLE = {
  ok: 'All checks passed', warn: 'Some checks need attention',
  fail: 'Some checks failed', busy: 'Running checks\u2026',
};
function setDoctorIcon(state) {
  const b = document.getElementById('doctorbtn');
  b.className = 'iconbtn ' + state;
  b.innerHTML = SEV_GLYPH[state] || '\u2026';
  b.title = SEV_TITLE[state] || 'System health';
  b.setAttribute('aria-label', b.title);
}
async function fetchDoctor() {
  if (doctorBusy) return;
  doctorBusy = true;
  setDoctorIcon('busy');
  if (currentView() === 'doctor') renderDoctor();
  try {
    doctor = await api('/api/doctor');
    doctorErr = null;
  } catch (err) {
    doctorErr = err.message;
  }
  doctorBusy = false;
  setDoctorIcon(doctor && !doctorErr ? doctor.aggregate : 'fail');
  if (currentView() === 'doctor') renderDoctor();
}
function sevDot(sev) { return '<span class="sev ' + esc(sev) + '" title="' + esc(sev) + '"></span>'; }
function renderDoctor() {
  const el = document.getElementById('view-doctor');
  const bar = '<div class="doctor-bar">'
    + '<a class="btn ghost" href="#/settings">\u2190 Settings</a>'
    + '<span class="hint" style="margin-left:auto">'
    + (doctorBusy ? 'running checks\u2026'
      : doctor ? 'checked ' + new Date(doctor.generated_at * 1000).toLocaleTimeString() : '')
    + '</span>'
    + '<button class="btn" type="button" id="rerunbtn"' + (doctorBusy ? ' disabled' : '') + '>Re-run checks</button>'
    + '</div>';
  let body;
  if (doctorErr) {
    body = '<p class="privacy-note">Could not run the checks: ' + esc(doctorErr) + '</p>';
  } else if (!doctor) {
    body = '<p class="hint">Running checks\u2026</p>';
  } else {
    body = doctor.sections.map((s) => {
      const worst = s.checks.some((c) => c.severity === 'fail') ? 'fail'
        : s.checks.some((c) => c.severity === 'warn') ? 'warn' : 'ok';
      const rows = s.checks.map((c) =>
        '<div class="row"><div class="info"><div class="lbl">' + sevDot(c.severity) + ' ' + esc(c.label) + '</div>'
        + (c.detail ? '<div class="desc mono">' + esc(c.detail) + '</div>' : '') + '</div></div>').join('');
      return '<details class="sec dsec"' + (worst === 'ok' ? '' : ' open') + '>'
        + '<summary><span class="chev">\u25b6</span><span class="t">' + esc(s.title) + '</span>'
        + '<span class="sum">' + sevDot(worst) + '</span></summary>'
        + '<div class="body">' + rows + '</div></details>';
    }).join('');
  }
  el.innerHTML = bar + body;
  const b = el.querySelector('#rerunbtn');
  if (b) b.addEventListener('click', fetchDoctor);
}

// ---------- search + theme ----------
function applyFilter(q) {
  q = (q || '').trim().toLowerCase();
  // Scoped to the settings view — doctor sections are not searchable.
  document.querySelectorAll('#view-settings details.sec').forEach((d) => {
    const hit = !q || d.textContent.toLowerCase().includes(q);
    d.style.display = hit ? '' : 'none';
    if (q && hit) d.open = true;
  });
}

// ---------- init ----------
async function init() {
  document.getElementById('q').addEventListener('input', (e) => applyFilter(e.target.value));
  document.getElementById('themebtn').addEventListener('click', () => {
    const light = document.documentElement.toggleAttribute('data-theme');
    if (light) document.documentElement.setAttribute('data-theme', 'light');
    try { localStorage.setItem('fono-theme', light ? 'light' : 'dark'); } catch (e) { /* private mode */ }
  });
  try { if (localStorage.getItem('fono-theme') === 'light') document.documentElement.setAttribute('data-theme', 'light'); } catch (e) { /* private mode */ }
  document.getElementById('savebtn').addEventListener('click', saveAll);
  document.getElementById('discardbtn').addEventListener('click', discardAll);
  showView();
  fetchDoctor(); // fire-and-forget: sets the header health icon
  try {
    const [c, m, v] = await Promise.all([
      api('/api/config'),
      api('/api/meta'),
      api('/api/vocabulary').catch(() => null),
    ]);
    cfg = c; orig = clone(c); meta = m;
    vocab = v; vocabOrig = v == null ? null : clone(v);
    document.getElementById('verchip').textContent =
      currentView() + ' \u00b7 v' + (meta.version || '');
    document.getElementById('cfgpath').textContent = meta.config_path || '';
    renderAll();
    loadApiKeys(); // fire-and-forget: fills the API Keys section
    loadSpeakers(); // fire-and-forget: fills the Speakers section
  } catch (err) {
    document.getElementById('loading').textContent = 'Could not load configuration: ' + err.message
      + (TOKEN ? '' : ' \u2014 if a token is configured, open this page as /?token=\u2026');
  }
}
init();
