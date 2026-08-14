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
  ['openrouter', 'OpenRouter', 'gpt-5.4-nano'],
];
const ASSISTANT_PROVIDERS = [
  ['openai', 'OpenAI', 'gpt-5.4-mini'], ['anthropic', 'Anthropic', 'claude-haiku-4-5'],
  ['gemini', 'Gemini', 'gemini-flash-lite-latest'], ['groq', 'Groq', 'gpt-oss-120b'],
  ['cerebras', 'Cerebras', 'zai-glm-4.7'], ['openrouter', 'OpenRouter', 'claude-haiku-4.5'],
];
const TTS_PROVIDERS = [
  ['openai', 'OpenAI', 'tts-1'], ['elevenlabs', 'ElevenLabs', 'eleven_v3'],
  ['cartesia', 'Cartesia', 'sonic-3.5'], ['deepgram', 'Deepgram', 'aura-2-thalia-en'],
  ['groq', 'Groq', 'orpheus-v1-english'], ['gemini', 'Gemini', 'flash-tts-preview'],
  ['speechmatics', 'Speechmatics', 'preview'], ['openrouter', 'OpenRouter', 'grok-voice-tts-1.0'],
];
// Cloud-only provider grids for Cleanup and the Assistant. `local`
// (embedded model) and `network` (self-hosted OpenAI-compatible server)
// are their own segments, so they never appear as "Cloud" cards.
const POLISH_CLOUD_PROVIDERS = POLISH_PROVIDERS.filter((p) => p[0] !== 'local');
const ASSISTANT_CLOUD_PROVIDERS = ASSISTANT_PROVIDERS.slice();
// Default endpoint offered when switching Cleanup / Assistant to the
// Network segment. Ollama's port, because it is the most common local
// server — but any OpenAI-compatible endpoint works.
const LOCAL_SERVER_URL = 'http://localhost:11434/v1/chat/completions';
// Shared copy for the Network segment. Deliberately engine-neutral:
// the backend speaks plain OpenAI chat-completions, so anything that
// serves that shape is supported.
const NETWORK_HINT = 'Connects to any OpenAI-compatible server on your network \u2014 Ollama, llama.cpp, LM Studio, vLLM, LocalAI, LiteLLM.';
const NETWORK_URL_HINT = 'Paste what your server prints at startup \u2014 e.g. http://localhost:11434. The /v1/chat/completions path is added for you.';
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
// `[<role>.cloud]` for the two LLM roles. Unlike STT/TTS there is no
// `provider` field: `<role>.backend` names the provider, so the sub-table
// only carries the model id and the secret name.
function ensureLlmCloud(base, provider) {
  if (!get(cfg, base) || typeof get(cfg, base) !== 'object') set(cfg, base, { api_key_ref: '', model: '' });
  // A model name is provider-specific: drop it when the provider changes.
  if (gv(base + '.api_key_ref', '') !== (ENV[provider] || '')) set(cfg, base + '.model', '');
  set(cfg, base + '.api_key_ref', ENV[provider] || '');
  if (get(cfg, base + '.model') === undefined) set(cfg, base + '.model', '');
}
// `[<role>.network]` — a self-hosted OpenAI-compatible server. Seeds a
// sensible default URL the first time the segment is entered, and never
// clobbers a URL the user already typed.
function ensureNetwork(base) {
  if (!get(cfg, base) || typeof get(cfg, base) !== 'object') {
    set(cfg, base, { url: '', model: '', api_key_ref: '' });
  }
  if (!gv(base + '.url', '')) set(cfg, base + '.url', LOCAL_SERVER_URL);
  if (get(cfg, base + '.model') === undefined) set(cfg, base + '.model', '');
  if (get(cfg, base + '.api_key_ref') === undefined) set(cfg, base + '.api_key_ref', '');
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
// Cleanup / Assistant backend → segment. `backend` is now the single
// source of truth: `local` = embedded GGUF, `network` = self-hosted
// OpenAI-compatible server, anything else = a cloud provider.
function llmSeg(base, dflt) {
  const b = gv(base + '.backend', dflt);
  return b === 'local' || b === 'network' ? b : 'cloud';
}
function polishSeg() { return llmSeg('polish', 'local'); }
function assistantSeg() { return llmSeg('assistant', 'none'); }
// Where a role should run when it is switched on without a backend
// chosen. Mirrors `resolve_llm_backend` in fono-core/src/providers.rs
// — a configured server first, then a saved cloud key in the same
// preference order, then the on-device model, which always works.
// Keep the order below in step with `LLM_AUTOSELECT_ORDER` there.
const LLM_AUTOSELECT_ORDER = ['groq', 'cerebras', 'openai', 'anthropic', 'gemini', 'openrouter'];
function autoBackend(base) {
  if (gv(base + '.network.url', '').trim()) return 'network';
  const saved = LLM_AUTOSELECT_ORDER.find((p) => meta && meta.secrets && meta.secrets[ENV[p]]);
  return saved || 'local';
}
// One-line description of a role's network target for section summaries.
function netSummary(base) {
  const url = gv(base + '.network.url', '');
  if (!url) return 'Network \u00b7 no server';
  // Show host[:port] only — the full URL is too long for a summary line.
  let host = url.replace(/^[a-z]+:\/\//i, '').split('/')[0];
  const model = gv(base + '.network.model', '');
  return 'Network \u00b7 ' + host + (model ? ' \u00b7 ' + model : '');
}
// Config paths for the two LLM roles. Written out in full rather than
// assembled from `base + '.network.url'` so the dotted paths are greppable:
// the coverage test in web_settings/mod.rs proves every config key is
// reachable from this page by searching for its literal path.
const LLM_PATHS = {
  polish: {
    backend: 'polish.backend',
    localModel: 'polish.local.model',
    cloudModel: 'polish.cloud.model',
    cloudKey: 'polish.cloud.api_key_ref',
    netUrl: 'polish.network.url',
    netModel: 'polish.network.model',
    netKey: 'polish.network.api_key_ref',
  },
  assistant: {
    backend: 'assistant.backend',
    localModel: 'assistant.local.model',
    cloudModel: 'assistant.cloud.model',
    cloudKey: 'assistant.cloud.api_key_ref',
    netUrl: 'assistant.network.url',
    netModel: 'assistant.network.model',
    netKey: 'assistant.network.api_key_ref',
  },
};

// Embedded local-LLM panel for the Cleanup / Assistant "Local" segment.
// Shows the current on-device GGUF model id and lets the user change it.
// `base` is 'polish' or 'assistant'.
function localLlmPanel(base) {
  const P = LLM_PATHS[base];
  const ids = (meta && meta.llm_local && meta.llm_local.models) || [];
  const cur = gv(P.localModel, '');
  let ctl;
  if (ids.length) {
    // A dropdown of what is actually installed, plus the current value if
    // it is something else — a typo used to surface only at first use.
    const opts = [['', 'Default (' + ids[0].id + ')']].concat(ids.map((m) => {
      const gb = (m.approx_mb / 1024).toFixed(1) + ' GB';
      return [m.id, m.id + ' \u00b7 ' + (m.installed ? 'installed' : 'downloads ' + gb)];
    }));
    if (cur && !ids.some((m) => m.id === cur)) opts.push([cur, cur + ' \u00b7 custom']);
    ctl = sel(P.localModel, opts, '');
  } else {
    ctl = txt(P.localModel, { mono: true, w: 220, ph: 'gemma-4-e2b' });
  }
  return row('Model', 'Runs on this machine. Anything not yet downloaded is fetched on first use.', ctl)
    + '<p class="hint" style="margin-top:6px">No API key needed \u2014 nothing leaves your computer.</p>';
}

// Self-hosted-server panel for the Cleanup / Assistant "Network" segment.
// Engine-agnostic: anything that answers OpenAI-shaped chat completions
// works, so nothing here names a specific product. "Test connection" asks
// the daemon (not the browser \u2014 the box is usually LAN-side) to fetch the
// server's model list, which then replaces the free-text model field.
function networkLlmPanel(base) {
  const P = LLM_PATHS[base];
  const models = netModels[base] || null;
  const cur = gv(P.netModel, '');
  let modelCtl;
  if (models && models.length) {
    const opts = [['', 'Server default']].concat(models.map((m) => [m, m]));
    if (cur && !models.includes(cur)) opts.push([cur, cur + ' \u00b7 not on server']);
    modelCtl = sel(P.netModel, opts, '');
  } else {
    modelCtl = txt(P.netModel, { mono: true, w: 220, ph: 'gemma4:12b' });
  }
  return row('Server address', NETWORK_URL_HINT, txt(P.netUrl, { mono: true, w: 320 }))
    + row('Test connection', 'Checks the address and lists the models it serves.',
      '<button class="btn" type="button" data-llm-probe="' + base + '">Test connection</button>'
      + ' <span class="hint llm-probe-status">' + esc(netStatus[base] || '') + '</span>')
    + row('Model', 'Which model on that server to use.', modelCtl)
    + row('API key name', 'Only for gateways that require a bearer token. Most local servers need none \u2014 leave empty.',
      txt(P.netKey, { mono: true, w: 220, ph: 'none' }))
    + '<p class="hint" style="margin-top:6px">' + NETWORK_HINT + '</p>';
}

// Per-role results of the last "Test connection" click. Ephemeral: never
// written into cfg, and kept at module scope so a section re-render does
// not throw away a model list the user just fetched.
const netModels = {};
const netStatus = {};

// ---------- local TTS engine + voice picker ----------
// Renders the engine card row (supertonic/piper/kokoro from /api/meta)
// plus a per-engine preset-voice dropdown, falling back to a free-text
// catalog-id field when the engine exposes no preset voice list.
function ttsLocalPanel() {
  const engines = (meta && meta.tts_local && meta.tts_local.engines) || [];
  const eng = gv('tts.local.engine', 'supertonic');
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
  // Supertonic exposes two extra knobs; the other engines ignore them.
  if (eng === 'supertonic') {
    const steps = gv('tts.local.num_steps', 5);
    out += row('Extra passes',
      'Runs 10 refinement passes instead of 5 for a small quality margin at higher latency. Off is plenty for most voices.',
      '<input type="checkbox" class="toggle" data-bind="tts.local.num_steps" data-kind="toggle" data-on="10" data-off="5"'
      + (Number(steps) >= 10 ? ' checked' : '') + ' />');
    const spd = Number(gv('tts.local.speed', 1)) || 1;
    out += row('Speed',
      'Speaking rate: slower \u00b7 normal \u00b7 faster.',
      '<span class="spd-out mono">' + spd.toFixed(1) + '\u00d7</span> '
      + '<input type="range" class="slider spd-slider" min="0.8" max="1.2" step="0.2" '
      + 'data-bind="tts.local.speed" data-kind="float" value="' + spd + '" />');
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
  polish(v) { llmSegPick('polish', POLISH_CLOUD_PROVIDERS, v); },
  assistant(v) { llmSegPick('assistant', ASSISTANT_CLOUD_PROVIDERS, v); },
};

// Cleanup and the Assistant share one backend enum, so they share one
// segment handler. `backend` is the only thing that decides the mode;
// the `[<role>.cloud]` and `[<role>.network]` sub-tables just hold the
// details for whichever mode is active and are left alone otherwise, so
// flipping between segments never loses a typed URL or API model id.
function llmSegPick(base, grid, v) {
  if (v === 'local') {
    set(cfg, base + '.backend', 'local');
  } else if (v === 'network') {
    ensureNetwork(base + '.network');
    set(cfg, base + '.backend', 'network');
  } else {
    const prev = gv(base + '.backend', '');
    const p = grid.some((x) => x[0] === prev) ? prev : 'openai';
    ensureLlmCloud(base + '.cloud', p);
    set(cfg, base + '.backend', p);
  }
}

// Provider-card click handlers. The explicit `.api_key_ref` sets
// duplicate the ensure* work on purpose: the coverage test in
// web_settings/mod.rs greps this file for full dotted paths.
const PICK = {
  'stt-provider'(v) { ensureSttCloud(v); set(cfg, 'stt.backend', v); },
  'polish-provider'(v) {
    ensureLlmCloud('polish.cloud', v);
    set(cfg, 'polish.cloud.api_key_ref', ENV[v] || '');
    set(cfg, 'polish.backend', v);
  },
  'assistant-provider'(v) {
    ensureLlmCloud('assistant.cloud', v);
    set(cfg, 'assistant.cloud.api_key_ref', ENV[v] || '');
    set(cfg, 'assistant.backend', v);
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
    if (gv('tts.local.engine', 'supertonic') !== v) set(cfg, 'tts.local.voice', '');
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
      if (s === 'local') return 'Local model \u00b7 ' + (gv('polish.local.model', '') || 'default');
      if (s === 'network') return netSummary('polish');
      return 'Cloud \u00b7 ' + pname(POLISH_CLOUD_PROVIDERS, gv('polish.backend', ''));
    },
    html() {
      const on = gv('polish.enabled', false);
      const s = polishSeg();
      let panel = '';
      if (s === 'local') {
        panel = localLlmPanel('polish');
      } else if (s === 'network') {
        panel = networkLlmPanel('polish');
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
        + row('Where it runs', 'On this machine, through a cloud provider, or on your own server.',
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
      if (s === 'local') str = 'Local model \u00b7 ' + (gv('assistant.local.model', '') || 'default');
      else if (s === 'network') str = netSummary('assistant');
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
        panel = networkLlmPanel('assistant');
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
        + row('Where it runs', 'On this machine, through a cloud provider, or on your own server.',
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
        + row('Save assistant conversations', 'Keep a transcript of what you asked the assistant and what it replied.', toggle('conversations.enabled', true))
        + row('Conversation retention', '', num('conversations.retention_days', 90, 'days'))
        + row('New conversation after', 'Silence this long starts a fresh conversation instead of continuing the last one.', num('conversations.idle_timeout_minutes', 5, 'min'))
        + '<p class="privacy-note">Audio never leaves this machine unless you pick a cloud provider. Browse or delete everything saved here on the <a href="#/history">History page</a>.</p>';
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
    id: 'tools', title: 'Tools & actions',
    summary() {
      if (!gv('assistant.tools.enabled', false)) return 'off';
      if (toolsErr) return 'unavailable';
      if (!toolsData) return 'loading\u2026';
      const t = toolsData.tools || [];
      if (!t.length) return 'nothing discovered yet';
      const on = t.filter((x) => x.enabled && x.available).length;
      return on + ' of ' + t.length + ' in use';
    },
    html() { return toolsHtml(); },
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
// Sample-manager state: which speaker's utterances are expanded, and the last
// loaded utterance list (rows + suggested_prune + floor) for that speaker.
let spkManageId = null, spkUtts = null, spkUttsErr = null;
function refreshSpeakersSection() {
  const sec = FONO_SECTIONS.find((s) => s.id === 'speakers');
  if (sec && document.getElementById('d-speakers')) {
    renderSection(sec);
    // Re-renders rebuild the device <select>s from static HTML, so refill them
    // and reveal the picker only when there's more than one microphone.
    spkPopulateDevices(null, 'spk-enroll-device');
    spkPopulateDevices(null, 'spk-cal-device');
  }
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
    + '<select id="spk-enroll-device" class="select enroll-device spk-hidden"><option value="">Default microphone</option></select>'
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
    + keyIconBtn('spk-manage', s.id, '\u2699', 'Manage samples')
    + keyIconBtn('spk-rename', s.id, '\u270E', 'Rename')
    + keyIconBtn('spk-delete', s.id, '\u2715', 'Delete', 'danger')
    + '</td></tr>';
  }).join('');
  return out + '<table class="key-table"><thead><tr>'
    + '<th>Name</th><th>Utterances</th><th>Strength</th><th>Calibrated</th><th>Updated</th><th></th>'
    + '</tr></thead><tbody>' + rows + '</tbody></table>'
    + manageSamplesHtml()
    + calibrateCardHtml();
}

// ---------- sample manager (per-utterance list + suggested prune) ----------
// Expanded from a roster row's gear button. Lists each enrolled clip with its
// capture-time quality metrics and on-demand consistency score, flags the ones
// a suggested prune would drop, and lets the user remove individual clips or
// accept the whole suggestion. The coverage floor (enforced server-side) means
// a prune can only ever tighten quality, never leave a profile under-enrolled.
function manageSamplesHtml() {
  if (spkManageId == null) return '';
  const sp = speakers && speakers.find((s) => s.id === spkManageId);
  if (!sp) return '';
  let body;
  if (spkUttsErr) body = '<p class="err">' + esc(spkUttsErr) + '</p>';
  else if (!spkUtts) body = '<p class="hint">Loading samples\u2026</p>';
  else {
    const prune = spkUtts.suggested_prune || [];
    const rows = (spkUtts.utterances || []).map((u) => {
      const flagged = prune.indexOf(u.id) >= 0;
      const dur = typeof u.duration_secs === 'number' ? u.duration_secs.toFixed(1) + '\u00a0s' : '\u2014';
      const snr = typeof u.snr_db === 'number' ? Math.round(u.snr_db) + '\u00a0dB' : '\u2014';
      const loud = typeof u.loudness_dbfs === 'number' ? Math.round(u.loudness_dbfs) + '\u00a0dBFS' : '\u2014';
      const cons = typeof u.consistency === 'number' ? u.consistency.toFixed(2) : '\u2014';
      return '<tr' + (flagged ? ' class="utt-weak"' : '') + '>'
        + '<td>' + esc(u.capture_source || '\u2014') + '</td>'
        + '<td>' + dur + '</td><td>' + loud + '</td><td>' + snr + '</td><td>' + cons + '</td>'
        + '<td>' + (flagged ? '<span class="spk-strength weak">weak</span>' : '<span class="hint">\u2713</span>') + '</td>'
        + '<td class="key-actions">' + keyIconBtn('spk-utt-del', u.id, '\u2715', 'Remove', 'danger') + '</td>'
        + '</tr>';
    }).join('');
    const table = '<table class="key-table"><thead><tr>'
      + '<th>Source</th><th>Length</th><th>Level</th><th>SNR</th><th>Match</th><th></th><th></th>'
      + '</tr></thead><tbody>' + rows + '</tbody></table>';
    const pruneBtn = prune.length
      ? '<button class="btn danger" data-spk-prune="' + sp.id + '" type="button">Remove '
        + prune.length + ' weak ' + (prune.length === 1 ? 'sample' : 'samples') + '</button>'
      : '<p class="hint">No weak samples to prune \u2014 your profile looks clean.</p>';
    body = table + '<div class="enroll-row">' + pruneBtn + '</div>';
  }
  return '<div class="enroll-card">'
    + '<div class="cal-title">Samples for ' + esc(sp.name)
    + ' <button class="btn" data-spk-manage-close type="button">Close</button></div>'
    + body + '</div>';
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
    + '<select id="spk-cal-device" class="select enroll-device spk-hidden"><option value="">Default microphone</option></select>'
    + '<button class="btn" id="spk-cal-record" data-spk-cal-record type="button">Record clip</button>'
    // Only surface Run test once there's a clip to run; keep it disabled (with a
    // reason) until the two-clip minimum is met, and hide Clear when empty.
    + (n >= 1
      ? '<button class="btn primary" data-spk-cal-run type="button"'
        + (canRun ? '' : ' disabled title="Record at least 2 clips first"') + '>Run test</button>'
        + '<button class="btn" data-spk-cal-clear type="button"' + (spkCalBusy ? ' disabled' : '') + '>Clear</button>'
      : '')
    + '</div>'
    + '<div id="spk-cal-meter" class="spk-meter spk-hidden"><div id="spk-cal-meter-bar" class="spk-meter-bar"></div></div>'
    + '<p id="spk-cal-status" class="hint">'
    + (n
      ? (n + (n === 1 ? ' clip' : ' clips') + ' captured'
        + (n < 2
          ? ' \u2014 record one more so Fono can measure how consistently it recognises you (2 clips minimum).'
          : ' \u2014 Run test when ready.'))
      : 'No test clips yet \u2014 record a few short new clips to begin.')
    + '</p>'
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
  const auto = calAutoThreshold(r);
  const fixed = calFixedThreshold(r);
  const safe = typeof r.far_threshold === 'number' ? r.far_threshold : null;
  return '<div class="cal-results">'
    + calHistogramSvg(g, im, fixed, auto, safe)
    + '<div class="cal-verdict"><span class="spk-strength ' + v.cls + '">' + v.label + '</span> '
    + '<strong>' + eerPct + '%</strong> equal-error rate' + lat + '</div>'
    + '<p class="hint">' + v.msg + '</p>'
    + '<p class="hint cal-help">Green bars are your own test clips scored against your saved voice; '
    + 'red bars are a large set of other people. Each bar counts how many clips landed at that '
    + 'similarity score \u2014 further right means more like your enrolled voice. The more the green '
    + 'sits to the right of the red, the more reliably Fono can tell you apart. Voices land in two '
    + 'clumps (yours near the right, everyone else near zero) because the score normalisation pushes '
    + 'them apart \u2014 the empty middle is the safety margin, and the accept cut-offs live in it. Bar '
    + 'heights are scaled within each group, so your handful of clips stays visible next to the large '
    + 'impostor set.</p>'
    + (auto != null && fixed != null && thr != null
      ? '<p class="hint cal-help">The vertical lines are candidate accept cut-offs \u2014 a clip scoring to '
        + 'the right of a line is accepted as you. <strong>Auto</strong> ('
        + auto.toFixed(3) + ') is what <code>threshold&nbsp;=&nbsp;"auto"</code> enforces at dictation time: '
        + 'it sits partway between your voice and the others and is re-derived live against the impostor set, '
        + 'so it adapts as your mic and room change. <strong>Fixed</strong> (' + fixed.toFixed(1) + ') is the '
        + 'value the button below pins \u2014 a rounded point set halfway between Auto and the measured '
        + 'balance point (equal-error ' + thr.toFixed(3) + '), so it stays predictable without clinging to the '
        + 'exact number these few clips produced and tolerates a slightly worse clip than the raw measured '
        + 'point would. '
        + (safe != null ? '<strong>Safety</strong> (' + safe.toFixed(3) + ') is the strict floor that keeps '
          + 'out about 99% of impostors; the fixed value never drops below it. ' : '')
        + 'A fixed threshold never adapts, though, so for tougher conditions prefer Auto and enroll a few '
        + 'clips from that mic and distance. Auto is the default and suits most people.</p>'
      : '')
    + '<div class="cal-legend"><span class="cal-swatch cal-genuine"></span>you ('
    + (r.genuine ? r.genuine.trials : 0) + ') <span class="cal-swatch cal-impostor"></span>others ('
    + (r.impostor ? r.impostor.trials : 0) + ')'
    + (safe != null ? ' <span class="cal-swatch cal-safe"></span>safety ' + safe.toFixed(3) : '')
    + (auto != null ? ' <span class="cal-swatch cal-auto"></span>auto ' + auto.toFixed(3) : '')
    + (fixed != null ? ' <span class="cal-swatch cal-thr"></span>fixed ' + fixed.toFixed(1) : '') + '</div>'
    + (fixed != null
      ? '<button class="btn" data-spk-cal-apply="' + fixed + '" type="button">Pin a fixed threshold ('
        + fixed.toFixed(1) + ')</button>'
      : '')
    + '</div>';
}
// Reproduce resolve_auto_threshold (speaker.rs) for the case a completed test
// always provides \u2014 both a genuine calibration and a live impostor set: the
// std-weighted midpoint between the genuine and impostor means, floored at the
// target-FAR operating point. This is the concrete value threshold = "auto"
// resolves to at dictation time, so the card can show it instead of leaving
// "auto" opaque.
function calAutoThreshold(r) {
  const g = r.genuine, im = r.impostor;
  if (!g || !im || typeof g.mean !== 'number' || typeof im.mean !== 'number') return null;
  const gStd = g.std || 0, iStd = im.std || 0, denom = gStd + iStd;
  const mid = denom > 0 ? (im.mean * gStd + g.mean * iStd) / denom : 0.5 * (g.mean + im.mean);
  const far = typeof r.far_threshold === 'number' ? r.far_threshold : mid;
  return Math.max(mid, far);
}
// The value the "Pin a fixed threshold" button writes: a rounded operating
// point set halfway between Auto and the measured equal-error point. Not the
// raw EER \u2014 pinning the exact number from 2\u20133 clips overfits and is
// fragile; a 1-decimal midpoint is predictable, forgives a slightly worse clip,
// and never dips below the target false-accept floor (rounded up to stay a clean
// number).
function calFixedThreshold(r) {
  const auto = calAutoThreshold(r);
  const eer = typeof r.eer_threshold === 'number' ? r.eer_threshold : null;
  if (auto == null || eer == null) return eer;
  let t = Math.round(((auto + eer) / 2) * 10) / 10;
  if (typeof r.far_threshold === 'number') t = Math.max(t, Math.ceil(r.far_threshold * 10) / 10);
  return t;
}
// Plain-language verdict bucketed by EER.
function calVerdict(eer) {
  if (eer <= 0.01) return { label: 'excellent', cls: 'strong', msg: 'Your voice is easy to tell apart here.' };
  if (eer <= 0.05) return { label: 'good', cls: 'strong', msg: 'Reliable separation on this mic and room.' };
  if (eer <= 0.10) return { label: 'fair', cls: 'ok', msg: 'Usable, but more or cleaner samples would help.' };
  return { label: 'weak', cls: 'weak', msg: 'Hard to separate \u2014 enroll more clips in your real environment.' };
}
// Inline-SVG overlaid histogram of the genuine vs impostor score distributions,
// with vertical markers at the safety, auto and fixed cut-offs. No chart library.
// Each group is scaled to its OWN peak (not a shared one) so your handful of
// clips stays visible beside the hundreds-strong impostor cohort; the chart
// shows where each group sits, not comparable raw counts.
function calHistogramSvg(genuine, impostor, fixed, auto, safe) {
  const all = genuine.concat(impostor);
  if (!all.length) return '';
  let lo = Math.min.apply(null, all), hi = Math.max.apply(null, all);
  [fixed, auto, safe].forEach((m) => { if (m != null) { lo = Math.min(lo, m); hi = Math.max(hi, m); } });
  if (hi - lo < 1e-6) { lo -= 0.5; hi += 0.5; }
  const pad = (hi - lo) * 0.05; lo -= pad; hi += pad;
  const W = 320, H = 96, BINS = 24;
  const bin = (hi - lo) / BINS;
  const gh = new Array(BINS).fill(0), ih = new Array(BINS).fill(0);
  const fill = (arr, dst) => arr.forEach((x) => {
    let k = Math.floor((x - lo) / bin); if (k < 0) k = 0; if (k >= BINS) k = BINS - 1; dst[k]++;
  });
  fill(genuine, gh); fill(impostor, ih);
  const bw = W / BINS;
  // Per-group peak + a floor so any non-empty bin is at least a few px tall.
  const bars = (arr, cls) => {
    const peak = Math.max(1, Math.max.apply(null, arr));
    return arr.map((c, k) => {
      if (!c) return '';
      const h = Math.max(4, c / peak * (H - 12));
      return '<rect class="' + cls + '" x="' + (k * bw).toFixed(1) + '" y="' + (H - h).toFixed(1)
        + '" width="' + (bw - 1).toFixed(1) + '" height="' + h.toFixed(1) + '"/>';
    }).join('');
  };
  const vline = (val, cls) => {
    if (val == null) return '';
    const x = ((val - lo) / (hi - lo) * W).toFixed(1);
    return '<line class="' + cls + '" x1="' + x + '" y1="0" x2="' + x + '" y2="' + H + '"/>';
  };
  return '<svg class="cal-hist" viewBox="0 0 ' + W + ' ' + H + '" preserveAspectRatio="none" role="img" '
    + 'aria-label="Score distribution of your voice versus others, with the safety, auto and fixed accept cut-offs">'
    + bars(ih, 'cal-impostor') + bars(gh, 'cal-genuine')
    + vline(safe, 'cal-safe-line') + vline(auto, 'cal-auto-line') + vline(fixed, 'cal-thr-line') + '</svg>'
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

// ---------- sample manager actions (per-utterance list + suggested prune) ----------
async function loadUtterances(id) {
  try {
    spkUtts = await api('/api/speakers/' + id + '/utterances');
    spkUttsErr = null;
  } catch (err) {
    spkUtts = null;
    spkUttsErr = err.message;
  }
  refreshSpeakersSection();
}
function spkManageOpen(id) {
  if (spkManageId === id) { spkManageClose(); return; }
  spkManageId = id;
  spkUtts = null;
  spkUttsErr = null;
  refreshSpeakersSection();
  loadUtterances(id);
}
function spkManageClose() {
  spkManageId = null;
  spkUtts = null;
  spkUttsErr = null;
  refreshSpeakersSection();
}
async function spkDeleteUtterance(uid) {
  if (spkManageId == null) return;
  if (!confirm('Remove this voice sample? This cannot be undone.')) return;
  try {
    await api('/api/speakers/' + spkManageId + '/utterances/' + uid, { method: 'DELETE' });
    toast('Sample removed');
    await loadUtterances(spkManageId);
    await loadSpeakers();
  } catch (err) { toast('Could not remove sample: ' + err.message, true); }
}
async function spkPrune(id) {
  const prune = (spkUtts && spkUtts.suggested_prune) || [];
  if (!prune.length) return;
  if (!confirm('Remove ' + prune.length + ' weak ' + (prune.length === 1 ? 'sample' : 'samples')
    + '? Your profile keeps its stronger samples. This cannot be undone.')) return;
  try {
    for (const uid of prune) {
      await api('/api/speakers/' + id + '/utterances/' + uid, { method: 'DELETE' });
    }
    toast('Removed ' + prune.length + ' weak ' + (prune.length === 1 ? 'sample' : 'samples'));
    await loadUtterances(id);
    await loadSpeakers();
  } catch (err) { toast('Could not prune samples: ' + err.message, true); }
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
    // Nothing to choose between with a single mic — keep the picker hidden.
    devEl.classList.toggle('spk-hidden', mics.length <= 1);
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

// ---------- voice-triggered actions (GET/PATCH /api/tools, POST discover) ----------
// Fono asks each configured MCP server what it can do, remembers the answer,
// and lets you switch individual tools off. Everything discovered starts on;
// you only ever deselect. Fewer tools is both faster and more accurate, so
// the count is worth showing.
let toolsData = null, toolsErr = null, toolsBusy = false;
function refreshToolsSection() {
  const sec = FONO_SECTIONS.find((s) => s.id === 'tools');
  if (sec && document.getElementById('d-tools')) renderSection(sec);
  // The settings summary and the page read one payload, so they are refreshed
  // together — a count that disagrees with the list it links to is exactly the
  // kind of quiet lie this page exists to prevent.
  if (currentView() === 'actions') renderActions();
}
async function loadTools() {
  try {
    toolsData = await api('/api/tools');
    toolsErr = null;
  } catch (err) {
    toolsData = null;
    toolsErr = err.message;
  }
  refreshToolsSection();
}
// How confidently Fono can tell whether a call actually did anything. This
// is not cosmetic: a tool it cannot check may never be replayed from a
// learned shortcut, and Fono will say "sent", never "done".
// Mirrors fono_core::config::derive_token_ref. The name is derived, not
// typed, so there is no second field whose only job is to agree with the
// first — and no way to mistype it into a token that reads as "not set".
function deriveTokenRef(name) {
  let out = '';
  for (const ch of name) {
    if (/[A-Za-z0-9]/.test(ch)) out += ch.toUpperCase();
    else if (!out.endsWith('_')) out += '_';
  }
  const base = out.replace(/^_+|_+$/g, '');
  if (!base) return '';
  // Secret names must start with a letter, or the server refuses to store it.
  return (/^[0-9]/.test(base) ? 'MCP_' : '') + base + '_TOKEN';
}
const TOOL_PROOF = {
  post_condition: ['checked', 'Fono re-reads the state afterwards to confirm it really happened.'],
  result_contract: ['reported', 'Fono relies on the server reporting a failure. It cannot see the result itself.'],
  none: ['unverified', 'Nothing can be checked \u2014 Fono can only say the request was sent.'],
};
function toolsHtml() {
  let out = row('Let the assistant control things', 'Discovers what your smart home and other connected services can do, '
    + 'and lets the assistant use them when you ask.', toggle('assistant.tools.enabled', false, 'tools'), 'master');
  const servers = (cfg.assistant && cfg.assistant.tools && cfg.assistant.tools.mcp) || [];
  const known = new Set((toolsData && toolsData.tools || []).map((t) => t.source));
  out += servers.map((s, i) => {
    const ref = s.auth_token_ref || deriveTokenRef(s.name || '');
    const seen = known.has(s.name);
    const tokenSet = !!(ref && meta && meta.secrets && meta.secrets[ref]);
    // The token row is only meaningful once the server has a name, since the
    // secret is filed under a name derived from it.
    const tokenPart = ref
      ? keyRow(ref, 'Access token', 'Write-only \u2014 the stored value is never shown. Filed under a name taken from the server\u2019s.')
      : row('Access token', 'Name the server first \u2014 its token is filed under a name taken from that.',
        '<span class="keystatus unset"><span class="dot"></span>Waiting for a name</span>');
    return '<div class="enroll-card"><div class="enroll-row">'
      + '<input class="input" data-bind="assistant.tools.mcp.name" data-idx="' + i + '" data-kind="text" placeholder="Name (e.g. Home Assistant)" value="' + esc(s.name || '') + '" style="width:170px" />'
      + '<input class="input mono" data-bind="assistant.tools.mcp.url" data-idx="' + i + '" data-kind="text" placeholder="http://homeassistant.local:8123" value="' + esc(s.url || '') + '" style="flex:1" />'
      + '<button class="btn" type="button" data-tool-try="' + i + '"' + (toolsBusy ? ' disabled' : '') + '>'
      + (toolsBusy ? 'Asking\u2026' : (seen ? 'Refresh' : 'Save & connect')) + '</button>'
      + '<button class="btn ghost" type="button" data-tool-srv-rm="' + i + '">Remove</button>'
      + '</div>'
      + tokenPart
      + (ref && !tokenSet ? '<p class="hint">Most servers need a token. Home Assistant: Profile \u2192 Security \u2192 Long-lived access tokens.</p>' : '')
      + '</div>';
  }).join('');
  out += '<div class="enroll-row"><button class="btn ghost" type="button" data-tool-srv-add>Add a server</button>'
    + (servers.length > 1
      ? '<button class="btn ghost" type="button" data-tool-discover' + (toolsBusy ? ' disabled' : '') + '>Refresh all</button>'
      : '')
    + '<span class="hint">Save &amp; connect checks the address before saving anything.</span></div>';

  if (toolsErr) return out + '<p class="privacy-note">Could not load tools: ' + esc(toolsErr) + '</p>';
  if (!toolsData) return out + '<p class="hint">Loading\u2026</p>';
  const tools = toolsData.tools || [];
  if (!tools.length) {
    return out + '<p class="hint">' + (servers.length
      ? 'Nothing found yet. Paste the token, then press Save &amp; connect.'
      : 'Add a server to see what it can do.') + '</p>';
  }
  // Deliberately a summary and a link, not the list. One server with a couple
  // of dozen tools already crowds this page out; five would bury every other
  // setting. The list, what each tool expects, and what the assistant is
  // actually told all live on their own page, where there is room to explain
  // them.
  const off = tools.filter((t) => !t.enabled).length;
  const missing = tools.filter((t) => !t.available).length;
  const bits = [tools.length + (tools.length === 1 ? ' thing' : ' things') + ' from '
    + srvCount(new Set(tools.map((t) => t.source)).size)];
  if (off) bits.push(off + ' switched off');
  if (missing) bits.push(missing + ' no longer offered');
  return out + row('Everything it can do', esc(bits.join(' \u00b7 '))
    + ' \u2014 open the list to see what each one expects, switch individual '
    + 'things off, and read the exact words the assistant is given.',
  '<a class="btn" href="#/actions">Open</a>');
}
function srvCount(n) { return n + (n === 1 ? ' server' : ' servers'); }

async function setToolEnabled(source, name, enabled) {
  try {
    await api('/api/tools', { method: 'PATCH', body: JSON.stringify({ source, name, enabled }) });
    const t = toolsData && (toolsData.tools || []).find((x) => x.source === source && x.name === name);
    if (t) t.enabled = enabled;
    refreshToolsSection();
  } catch (err) { toast('Could not change that tool: ' + err.message, true); }
}
// Save one server — but only after checking it actually answers.
//
// The check runs against what is currently typed and writes nothing, so a
// wrong address or a missing token can never leave a half-finished server
// behind. Only once the server replies do we save the config and fold what
// it offers into the catalogue. Saving something known-broken is the
// failure mode worth designing out.
async function tryServer(i) {
  const s = (gv('assistant.tools.mcp', [])[i]) || {};
  if (!(s.name || '').trim()) { toast('Give the server a name first.', true); return; }
  if (!(s.url || '').trim()) { toast('Give the server an address first.', true); return; }
  toolsBusy = true;
  refreshToolsSection();
  try {
    const p = await api('/api/tools/discover', {
      method: 'POST',
      body: JSON.stringify({ name: s.name, url: s.url, auth_token_ref: s.auth_token_ref || '' }),
    });
    await saveAll();
    await api('/api/tools/discover', { method: 'POST' });
    // The token's name is derived from the server's, so saving a rename can
    // change which secret this row reports on. Re-read rather than guess.
    try { meta = await api('/api/meta'); } catch (_) { /* keep the old view */ }
    toast('Connected to ' + (p.server_name || s.name) + ' \u2014 ' + p.count
      + (p.count === 1 ? ' thing it can do' : ' things it can do'));
  } catch (err) {
    toast((s.name || 'Server') + ': ' + err.message, true);
  }
  toolsBusy = false;
  await loadTools();
}

// "Test connection" for a self-hosted LLM server. Asks the daemon to fetch
// the model list (the browser usually cannot reach a LAN box), then swaps
// the free-text model field for a dropdown of what the server actually
// serves. Purely local to the page: nothing is saved by testing.
async function probeLlmServer(base, sec) {
  const url = gv(base + '.network.url', '').trim();
  if (!url) { toast('Enter a server address first.', true); return; }
  netStatus[base] = 'Connecting\u2026';
  if (sec) renderSection(sec);
  try {
    const r = await api('/api/llm/probe', {
      method: 'POST',
      body: JSON.stringify({ url: url, api_key_ref: gv(base + '.network.api_key_ref', '') }),
    });
    netModels[base] = r.models || [];
    netStatus[base] = r.count + (r.count === 1 ? ' model available' : ' models available');
    // Nothing chosen yet and the server offers exactly one model: pick it.
    // Saves a click in the overwhelmingly common single-model setup.
    if (!gv(base + '.network.model', '') && netModels[base].length === 1) {
      set(cfg, base + '.network.model', netModels[base][0]);
    }
  } catch (err) {
    netModels[base] = null;
    netStatus[base] = err.message;
  }
  if (sec) renderSection(sec);
  updateBar();
}

async function discoverTools() {
  toolsBusy = true;
  refreshToolsSection();
  try {
    const r = await api('/api/tools/discover', { method: 'POST' });
    const bad = (r.servers || []).filter((s) => s.error);
    const ok = (r.servers || []).filter((s) => !s.error);
    const found = ok.reduce((n, s) => n + (s.count || 0), 0);
    if (bad.length) toast(bad.map((s) => s.server + ': ' + s.error).join('; '), true);
    else toast('Found ' + found + (found === 1 ? ' tool' : ' tools'));
  } catch (err) {
    toast('Could not reach the server: ' + err.message, true);
  }
  toolsBusy = false;
  await loadTools();
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
  if (el.dataset && el.dataset.toolToggle !== undefined) {
    setToolEnabled(el.dataset.toolSrc, el.dataset.toolName, el.checked);
    return;
  }
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
    case 'toggle':
      // A plain toggle writes a boolean. When data-on/data-off are present it
      // instead writes those numbers, so an on/off switch can drive a numeric
      // config field (e.g. Supertonic "extra passes" → num_steps 10/5).
      if (el.dataset.on !== undefined || el.dataset.off !== undefined) {
        v = el.checked ? Number(el.dataset.on) : Number(el.dataset.off);
      } else {
        v = el.checked;
      }
      break;
    case 'num': v = Math.max(0, parseInt(el.value, 10) || 0); break;
    case 'float': v = parseFloat(el.value) || 0; break;
    case 'radio': if (!el.checked) return; v = el.value; break;
    default: v = el.value;
  }
  set(cfg, boundPath(el), v);
  // Turning Cleanup or the Assistant on with no backend chosen used to
  // render an empty provider grid, which reads as broken. Pick the best
  // thing actually available, mirroring `resolve_llm_backend` in
  // fono-core so the page agrees with what the daemon would have done.
  if (v === true && (boundPath(el) === 'polish.enabled' || boundPath(el) === 'assistant.enabled')) {
    const base = boundPath(el).split('.')[0];
    if (gv(base + '.backend', 'none') === 'none') set(cfg, base + '.backend', autoBackend(base));
  }
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
  // Live "1.0×" readout next to the Supertonic speed slider.
  if (el.classList.contains('spd-slider') && el.previousElementSibling && el.previousElementSibling.classList.contains('spd-out')) {
    el.previousElementSibling.textContent = Number(el.value).toFixed(1) + '\u00d7';
  }
});

document.addEventListener('click', (e) => {
  const t = e.target.closest('[data-seg],[data-pick],[data-tts-test],[data-tag-rm],[data-wake-rm],[data-wake-add],[data-vocab-rm],[data-vocab-add],[data-keycap],[data-reset],[data-key-edit],[data-key-clear],[data-key-save],[data-key-cancel],[data-key-new],[data-key-rename],[data-key-revoke],[data-key-restore],[data-key-delete],[data-spk-rename],[data-spk-delete],[data-spk-record],[data-spk-submit],[data-spk-discard],[data-spk-cal-record],[data-spk-cal-run],[data-spk-cal-clear],[data-spk-cal-apply],[data-spk-manage],[data-spk-manage-close],[data-spk-utt-del],[data-spk-prune],[data-tool-discover],[data-tool-try],[data-tool-srv-add],[data-tool-srv-rm],[data-llm-probe]');
  if (!t) return;
  const secEl = t.closest('details.sec');
  const sec = secEl ? FONO_SECTIONS.find((s) => 'd-' + s.id === secEl.id) : null;

  if (t.dataset.toolDiscover !== undefined) { discoverTools(); return; }
  if (t.dataset.llmProbe) { probeLlmServer(t.dataset.llmProbe, sec); return; }
  if (t.dataset.toolTry !== undefined) { tryServer(Number(t.dataset.toolTry)); return; }
  if (t.dataset.toolSrvAdd !== undefined) {
    const arr = gv('assistant.tools.mcp', []).slice();
    arr.push({ name: '', url: '', auth_token_ref: '' });
    set(cfg, 'assistant.tools.mcp', arr);
    afterChange(t, sec);
    return;
  }
  if (t.dataset.toolSrvRm !== undefined) {
    const arr = gv('assistant.tools.mcp', []).slice();
    arr.splice(parseInt(t.dataset.toolSrvRm, 10), 1);
    set(cfg, 'assistant.tools.mcp', arr);
    afterChange(t, sec);
    return;
  }

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
      model = gv('tts.local.engine', 'supertonic');
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
  if (t.dataset.spkManage) { spkManageOpen(parseInt(t.dataset.spkManage, 10)); return; }
  if (t.dataset.spkManageClose !== undefined) { spkManageClose(); return; }
  if (t.dataset.spkUttDel) { spkDeleteUtterance(parseInt(t.dataset.spkUttDel, 10)); return; }
  if (t.dataset.spkPrune) { spkPrune(parseInt(t.dataset.spkPrune, 10)); return; }

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
    // Whichever page is showing, `/` means "let me type at the list in front
    // of me" — the settings search, or the filter over what the assistant can do.
    const box = document.getElementById(currentView() === 'actions' ? 'actq' : 'q');
    if (box) box.focus();
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
      // The tools payload is derived from the config the daemon just wrote —
      // the master switch, which servers exist, and the
      // exact words the assistant is given. Re-read it so the summary here
      // and the list it links to describe the config that is now live.
      loadTools();
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
// Five views share the page shell (header, toast, theme, token): the settings
// editor (default), the doctor report, the prompt-cache panel, the history
// browser, and the tools & actions list. Hash routing keeps `?token=…` intact
// across navigation — a real path would drop it.
function currentView() {
  if (location.hash === '#/doctor') return 'doctor';
  if (location.hash === '#/cache') return 'cache';
  if (location.hash === '#/history') return 'history';
  if (location.hash === '#/actions') return 'actions';
  return 'settings';
}
function showView() {
  const v = currentView();
  document.getElementById('view-settings').hidden = v !== 'settings';
  document.getElementById('view-doctor').hidden = v !== 'doctor';
  document.getElementById('view-cache').hidden = v !== 'cache';
  document.getElementById('view-history').hidden = v !== 'history';
  document.getElementById('view-actions').hidden = v !== 'actions';
  document.getElementById('verchip').textContent =
    v + (meta && meta.version ? ' \u00b7 v' + meta.version : '');
  if (v === 'doctor') renderDoctor();
  if (v === 'cache') {
    // Same reasoning as the actions view: paint what we have so the page never
    // flashes empty, then re-read. Arriving here is exactly when a stale copy
    // is worst — the shape changes on every turn.
    renderCache();
    loadCache();
  }
  if (v === 'history') openHistory();
  if (v === 'actions') {
    // Render what we already have so the page never flashes empty, then
    // always re-fetch. Arriving here is exactly when a stale copy hurts: the
    // master switch and the server list both live in the
    // settings editor, so anything changed there must be re-read on the way
    // in, or the page shows a state the daemon has already left behind.
    renderActions();
    loadTools();
  }
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
    + '<a class="btn ghost" href="#/cache">Prompt cache</a>'
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

// ---------- prompt cache ----------
// GET /api/promptcache: { caches: [{ role, model, runtime, max_entries,
// max_bytes, checkpoint_bytes, entries_pinned/evictable/free,
// bytes_pinned/evictable/free, bytes_resident, nodes, unplaced, verdicts,
// counters }] }. `nodes` arrives
// depth-first with a `depth` on each entry, so the tree draws by indentation
// with no client-side index.
//
// Fetched on entering #/cache and on explicit refresh. Cheap on the daemon
// side — no probes, no network — so unlike the doctor report this one is safe
// to re-read as often as the user likes.
let cacheData = null, cacheErr = null, cacheBusy = false, cacheRole = null;

async function loadCache() {
  if (cacheBusy) return;
  cacheBusy = true;
  if (currentView() === 'cache') renderCache();
  try {
    cacheData = (await api('/api/promptcache')).caches || [];
    cacheErr = null;
  } catch (err) {
    cacheErr = err.message;
  }
  cacheBusy = false;
  if (currentView() === 'cache') renderCache();
}

function fmtMiB(bytes) {
  const mib = (bytes || 0) / 1048576;
  if (mib >= 100) return mib.toFixed(0) + ' MiB';
  if (mib >= 1) return mib.toFixed(1) + ' MiB';
  return ((bytes || 0) / 1024).toFixed(0) + ' KiB';
}
function fmtIdle(secs) {
  if (secs < 5) return 'just now';
  if (secs < 60) return secs + 's ago';
  if (secs < 3600) return Math.round(secs / 60) + 'm ago';
  return Math.round(secs / 3600) + 'h ago';
}
// Recency as five steps rather than a continuous ramp: the useful question is
// "is this the branch I was just on, or a stale one", and five buckets answer
// it without asking anyone to compare two similar shades. Rank 0 is the
// coldest entry, so the ramp runs with the rank, not against it.
function heatClass(rank, total) {
  if (total <= 1) return 'h4';
  return 'h' + Math.min(4, Math.floor((rank / (total - 1)) * 4.999));
}

// Two segments against a budget: the pins first, hatched, then what eviction
// could actually reclaim. Both budgets count only reclaimable entries, so the
// pins are drawn *ahead of* the budget rather than inside it — charging them
// against it would show a bar over half full while the label read 2 of 10.
function occBar(pinned, used, budget, label, note) {
  const total = (budget + pinned) || 1;
  const pct = (n) => (100 * n / total).toFixed(2) + '%';
  return '<div class="pc-occ">'
    + '<div class="pc-occ-hd"><span class="pc-occ-lbl">' + esc(label) + '</span>'
    + '<span class="hint mono">' + esc(note) + '</span></div>'
    + '<div class="pc-bar">'
    + '<span class="pc-seg pin" style="width:' + pct(pinned) + '"></span>'
    + '<span class="pc-seg use" style="width:' + pct(used) + '"></span>'
    + '</div></div>';
}

// The layer names are internal (`f8_system`, `f7_context`). Say what each one
// actually holds; the raw name stays alongside for anyone reading the source.
const PC_LAYER = {
  f8_system: 'assistant instructions',
  f8_chat_prefix: 'conversation',
  history_prefix: 'conversation history',
  f7_system: 'cleanup instructions',
  f7_context: 'cleanup context',
  exact_prompt: 'one exact prompt',
};

// Chips are ordered by what the number costs, not by how alarming it sounds. A
// stranded pin is usually under a megabyte; repeated copies of the same prefix
// routinely run to hundreds, so that is the one the eye should land on.
function cacheChips(c) {
  const v = c.verdicts;
  const chip = (cls, n, t) =>
    '<span class="chip ' + cls + '"><span class="n">' + esc(n) + '</span> ' + esc(t) + '</span>';
  const plural = (n, one) => (n === 1 ? one : one + 's');
  const out = [];
  out.push(chip(v.fragmented ? 'chip-bad' : 'chip-ok', v.roots, plural(v.roots, 'root')));
  out.push(chip(v.heads_over_slots ? 'chip-bad' : 'chip-ok', v.heads,
    v.heads === 1 ? 'live branch' : 'live branches'));
  out.push(chip('', v.max_depth, v.max_depth === 1 ? 'level deep' : 'levels deep'));
  // Share, not a bare figure: "222 MiB" alone gives the reader no denominator.
  const share = c.bytes_resident
    ? Math.round(100 * v.duplication_bytes / c.bytes_resident) : 0;
  if (v.duplication_bytes) {
    out.push(chip(share >= 50 ? 'chip-bad' : '', share + '%',
      'repeated (' + fmtMiB(v.duplication_bytes) + ')'));
  }
  if (v.stranded_pins) out.push(chip('', v.stranded_pins, plural(v.stranded_pins, 'stranded pin')));
  if (v.orphans) out.push(chip('', v.orphans, 'unplaced'));
  let verdict = '';
  if (v.warming) {
    verdict = 'Nothing has used the cache yet \u2014 this is startup prewarm, so the shape means little.';
  } else if (v.heads_over_slots) {
    verdict = 'More live branches than slots (' + c.max_entries + '): they are now evicting each other.';
  } else if (share >= 50) {
    verdict = 'Each checkpoint is a whole copy of the prompt before it, so most of the memory'
      + ' here holds the same tokens over again. That is what limits how many conversations'
      + ' stay warm at once.';
  } else if (v.fragmented) {
    verdict = 'A branch does not descend from any pinned base, so it pays a cold prefill however warm the pins are.';
  } else if (v.stranded_pins) {
    verdict = 'A pinned entry has nothing growing off it \u2014 a slot and a blob spent on nothing.';
  }
  return '<div class="chips">' + out.join('') + '</div>'
    + (verdict ? '<p class="hint">' + esc(verdict) + '</p>' : '');
}

// Reuse rate is the headline, but a percentage off one or two turns says
// nothing, so below a real sample we show the raw counts instead of a
// confident-looking 100%. Zero evictions and zero prunes are the normal,
// healthy case and not worth a column each; a lost pin never is, so that one is
// always spelled out.
function cacheCounters(c) {
  const restores = c.restores || 0, cold = c.cold_prefills || 0;
  const seen = restores + cold;
  const reasons = Object.entries(c.cold_prefill_reasons || {})
    .map(([k, n]) => n + ' ' + k.replace(/_/g, ' ')).join(', ');
  const out = [];
  if (seen >= 5) {
    out.push('<span><b>' + Math.round(100 * restores / seen) + '%</b> of prompts reused a checkpoint</span>');
  } else if (seen === 0) {
    out.push('<span>no prompt has consulted the cache yet</span>');
  } else {
    out.push('<span><b>' + restores + ' of ' + seen + '</b> prompts reused a checkpoint'
      + ' — too few to rate</span>');
  }
  if (cold) out.push('<span>' + cold + ' started cold' + (reasons ? ' (' + esc(reasons) + ')' : '') + '</span>');
  if (c.evictions) out.push('<span>' + c.evictions + ' evicted</span>');
  if (c.prunes) out.push('<span>' + c.prunes + ' superseded</span>');
  out.push(c.pin_releases
    ? '<span class="bad">' + c.pin_releases + ' pinned prefixes lost</span>'
    : '<span>no pinned prefix lost</span>');
  return '<div class="pc-counters mono">' + out.join('') + '</div>' + rereadLine(c);
}

// Why a prompt was read again, in tokens rather than in lookups: one miss on a
// long conversation costs more than a hundred on short ones. Only one of these
// causes is a storage problem — a checkpoint that existed and was dropped —
// and it is the one that decides whether keeping checkpoints on disk would pay
// for itself, so it is named rather than lumped in with the rest.
const PC_REREAD = {
  deepest: 'nothing cached went any deeper',
  eviction: 'a checkpoint had been dropped to stay in budget',
  divergence: 'the prompt changed earlier than the cached one',
  runtime_key_change: 'the model or its settings changed',
};

function rereadLine(c) {
  const by = c.reread_prefix_tokens || {};
  const rows = Object.entries(by).filter(([, n]) => n > 0);
  if (!rows.length) return '';
  const total = rows.reduce((a, [, n]) => a + n, 0);
  rows.sort((a, b) => b[1] - a[1]);
  const items = rows.map(([k, n]) =>
    '<li><b>' + n.toLocaleString() + '</b> because ' + esc(PC_REREAD[k] || k.replace(/_/g, ' '))
    + '</li>').join('');
  // The summary has to carry the answer, not just the total. A bare count
  // behind a closed triangle reads as a footnote and gets scanned past — and
  // the cause is the whole point of counting. Named inline, and opened on
  // arrival when a dropped checkpoint is implicated, because that is the one
  // worth acting on.
  const dropped = by.eviction || 0;
  const head = dropped
    ? '<b>' + dropped.toLocaleString() + '</b> of ' + total.toLocaleString()
      + ' tokens were read a second time because a saved conversation had been dropped'
    : total.toLocaleString() + ' tokens have been read a second time \u2014 '
      + esc(PC_REREAD[rows[0][0]] || rows[0][0].replace(/_/g, ' '));
  return '<details class="pc-reread' + (dropped ? ' bad' : '') + '"' + (dropped ? ' open' : '')
    + '><summary>' + head + '</summary><ul>' + items + '</ul>'
    + '<p class="hint">Only the dropped-conversation line is work that keeping'
    + ' saved conversations on disk could have saved. The rest would have been read'
    + ' again whatever was stored.</p></details>';
}

function cacheRow(n, total) {
  const doomed = n.evicts_in === 1;
  const cls = ['pc-node', heatClass(n.lru_rank, total)];
  if (n.pinned) cls.push('pinned');
  if (doomed) cls.push('doomed');
  const tokens = n.parent ? '+' + n.delta_tokens : String(n.tokens);
  const fate = n.pinned ? 'pinned' : n.evicts_in === 1 ? 'out next' : 'out #' + n.evicts_in;
  // The prompt itself, not its fingerprint. A hash identifies an entry to the
  // cache and to nobody else; what a reader wants to know is which
  // conversation this is.
  const head = (PC_LAYER[n.layer] || n.layer) + ' \u00b7 ' + n.tokens + ' tokens \u00b7 '
    + fmtMiB(n.bytes);
  const title = n.preview
    ? head + '\n\n' + n.preview
    : head + (n.parent ? '\nextends ' + n.parent : '\nno cached ancestor');
  const name = PC_LAYER[n.layer] || n.layer;
  return '<div class="' + cls.join(' ') + '" style="--d:' + n.depth + '" title="' + esc(title) + '">'
    + '<span class="pc-name">' + (n.pinned ? '\u25c9 ' : '') + esc(name)
    + ' <span class="pc-raw mono">' + esc(n.layer) + '</span></span>'
    + '<span class="pc-tok mono">' + esc(tokens) + ' tok</span>'
    + '<span class="pc-by mono">' + esc(fmtMiB(n.bytes)) + '</span>'
    + '<span class="pc-when">' + esc(fmtIdle(n.idle_secs)) + '</span>'
    + '<span class="pc-fate mono">' + esc(fate) + '</span>'
    + '</div>';
}

// A budget in megabytes means nothing on its own: the same 1.3 GiB is room for
// three conversations on a small model and not one on a large one. The budget
// is set as a multiple of what one conversation costs, so say it that way.
function checkpointLine(c) {
  const one = c.checkpoint_bytes || 0;
  if (!one) return '';
  const room = Math.floor(c.max_bytes / one);
  const note = room >= 1
    ? 'room for ' + room + (room === 1 ? ' conversation' : ' conversations')
    : 'not enough for even one — the cache is off until the context is shorter';
  return '<p class="hint">One saved conversation costs ' + esc(fmtMiB(one)) + ' at this'
    + ' context: ' + esc(note) + '.</p>';
}

function cacheBody(c) {
  const total = c.nodes.length + c.unplaced.length;
  // The slot line has to reconcile with the tree below it, which shows every
  // entry. Reporting only the reclaimable count against the budget left a
  // "2 of 10" sitting above four rows.
  let out = occBar(c.entries_pinned, c.entries_evictable, c.max_entries, 'slots',
    total + (total === 1 ? ' entry' : ' entries') + ' \u00b7 ' + c.entries_pinned
    + ' pinned \u00b7 ' + c.entries_evictable + ' of ' + c.max_entries + ' reclaimable slots');
  out += occBar(c.bytes_pinned, c.bytes_evictable, c.max_bytes, 'memory',
    fmtMiB(c.bytes_resident) + ' held \u00b7 ' + fmtMiB(c.bytes_pinned) + ' pinned \u00b7 '
    + fmtMiB(c.bytes_evictable) + ' of ' + fmtMiB(c.max_bytes) + ' reclaimable budget');
  out += checkpointLine(c);
  out += cacheChips(c);
  out += cacheCounters(c.counters);
  if (!c.nodes.length && !c.unplaced.length) {
    out += '<p class="hint">Nothing cached yet. The base prompts are stored when the model'
      + ' first loads, and branches appear as you talk to it.</p>';
    return out;
  }
  out += '<div class="pc-tree">' + c.nodes.map((n) => cacheRow(n, total)).join('') + '</div>';
  if (c.unplaced.length) {
    out += '<details class="pc-unplaced"><summary>' + c.unplaced.length
      + ' reachable by exact match only</summary>'
      + '<div class="pc-tree">' + c.unplaced.map((n) => cacheRow(n, total)).join('') + '</div>'
      + '<p class="hint">These recorded no tokens, so a prompt that merely extends them'
      + ' cannot find them \u2014 only an identical one can.</p></details>';
  }
  out += '<p class="privacy-note">Every entry is a complete copy of the model\u2019s state at'
    + ' that point, never a link in a chain, which is what makes it safe to drop any one of'
    + ' them. A deeper entry therefore repeats everything above it.</p>';
  return out;
}

function renderCache() {
  const el = document.getElementById('view-cache');
  const list = cacheData || [];
  if (list.length && !list.some((c) => c.role === cacheRole)) cacheRole = list[0].role;
  const tabs = list.length > 1
    ? '<div class="pc-tabs">' + list.map((c) =>
      '<button class="btn' + (c.role === cacheRole ? ' primary' : ' ghost')
      + '" type="button" data-role="' + esc(c.role) + '">' + esc(c.role) + '</button>').join('')
    + '</div>'
    : '';
  const bar = '<div class="doctor-bar">'
    + '<a class="btn ghost" href="#/doctor">\u2190 Health</a>'
    + tabs
    + '<span class="hint" style="margin-left:auto">'
    + (cacheBusy ? 'reading\u2026' : '') + '</span>'
    + '<button class="btn" type="button" id="cachereload"'
    + (cacheBusy ? ' disabled' : '') + '>Refresh</button></div>';

  let body;
  if (cacheErr) {
    body = '<p class="privacy-note">Could not read the cache: ' + esc(cacheErr) + '</p>';
  } else if (!cacheData) {
    body = '<p class="hint">Reading\u2026</p>';
  } else if (!list.length) {
    body = '<p class="hint">No prompt cache to show. Only the built-in local models keep one'
      + ' \u2014 a cloud provider holds no state here.</p>';
  } else {
    const c = list.find((x) => x.role === cacheRole) || list[0];
    body = '<section class="sec"><div class="body">'
      + '<p class="hint mono">' + esc(c.model) + ' \u00b7 runtime ' + esc(c.runtime) + '</p>'
      + cacheBody(c) + '</div></section>';
  }
  el.innerHTML = bar + body;
  const r = el.querySelector('#cachereload');
  if (r) r.addEventListener('click', loadCache);
  el.querySelectorAll('[data-role]').forEach((b) => {
    b.addEventListener('click', () => { cacheRole = b.dataset.role; renderCache(); });
  });
}

// ---------- history browser ----------
// Two tabs over what Fono has saved locally: assistant conversations
// (GET /api/history/conversations) and dictation transcripts
// (GET /api/history/dictation). Both show the detected speaker when
// speaker verification made a match. Loaded on first visit to #/history
// and on explicit refresh — never polled.
//
// Conversations open first because they are the only tab you cannot read
// anywhere else: a transcript is a line of text the user watched being
// typed, while a conversation is the record of what the assistant was
// asked to do and whether it worked.
let histTab = 'conversations';
let histDict = null, histThreads = null, histErr = null, histBusy = false;
let histQuery = '';
// Thread ids the user has expanded, mapped to their loaded turns.
const histOpen = new Map();
let histSearchTimer = null;

function fmtWhen(ts) {
  if (!ts) return '';
  const d = new Date(ts * 1000);
  const today = new Date();
  const sameDay = d.toDateString() === today.toDateString();
  return sameDay ? d.toLocaleTimeString() : d.toLocaleString();
}

function openHistory() {
  // Render immediately from cache so switching back is instant, then
  // refresh in the background.
  renderHistory();
  if (histDict === null && histThreads === null) loadHistory();
}

async function loadHistory() {
  if (histBusy) return;
  histBusy = true;
  renderHistory();
  try {
    if (histTab === 'dictation') {
      const q = histQuery ? '&q=' + encodeURIComponent(histQuery) : '';
      histDict = (await api('/api/history/dictation?limit=100' + q)).entries || [];
    } else {
      histThreads = (await api('/api/history/conversations?limit=100')).threads || [];
    }
    histErr = null;
  } catch (err) {
    histErr = err.message;
  }
  histBusy = false;
  renderHistory();
}

async function toggleThread(id) {
  if (histOpen.has(id)) { histOpen.delete(id); renderHistory(); return; }
  histOpen.set(id, null); // placeholder → renders "loading"
  renderHistory();
  try {
    const res = await api('/api/history/conversations/' + id);
    histOpen.set(id, res.turns || []);
  } catch (err) {
    histOpen.set(id, []);
    toast('Could not open that conversation: ' + err.message, true);
  }
  renderHistory();
}

async function deleteHistory(kind, id) {
  const what = id ? 'this entry' : ('every saved ' + (kind === 'dictation' ? 'transcript' : 'conversation'));
  if (!window.confirm('Permanently delete ' + what + '?')) return;
  try {
    const res = await api('/api/history/' + kind + (id ? '/' + id : ''), { method: 'DELETE' });
    toast('Deleted ' + res.deleted + ' record' + (res.deleted === 1 ? '' : 's'));
    if (kind === 'dictation') histDict = null; else { histThreads = null; histOpen.clear(); }
    loadHistory();
  } catch (err) {
    toast('Could not delete: ' + err.message, true);
  }
}

function speakerChip(name) {
  return name ? '<span class="kbd" title="Detected speaker">' + esc(name) + '</span>' : '';
}

function renderDictationList() {
  if (!histDict) return '<p class="hint">Loading\u2026</p>';
  if (!histDict.length) {
    return '<p class="hint">' + (histQuery
      ? 'Nothing matches \u201c' + esc(histQuery) + '\u201d.'
      : 'No dictation saved yet. Transcripts appear here once you dictate '
        + '(unless you turned saving off in History &amp; Privacy).') + '</p>';
  }
  return histDict.map((e) => {
    const where = [e.app_class, e.app_title].filter(Boolean).join(' \u00b7 ');
    const backend = [e.stt_backend, e.language].filter(Boolean).join(' \u00b7 ');
    return '<div class="row hrow"><div class="info">'
      + '<div class="lbl">' + esc(e.cleaned || e.raw) + '</div>'
      + (e.cleaned && e.cleaned !== e.raw
        ? '<div class="desc mono">heard: ' + esc(e.raw) + '</div>' : '')
      + '<div class="desc">' + esc(fmtWhen(e.ts)) + speakerChipSuffix(e.speaker)
      + (where ? ' \u00b7 ' + esc(where) : '') + (backend ? ' \u00b7 ' + esc(backend) : '')
      + '</div></div>'
      + '<div class="ctl"><button class="btn ghost" type="button" data-hdel="' + e.id + '">Delete</button></div>'
      + '</div>';
  }).join('');
}

function speakerChipSuffix(name) {
  return name ? ' \u00b7 ' + speakerChip(name) : '';
}

function renderTurns(turns) {
  if (turns === null) return '<p class="hint">Loading\u2026</p>';
  if (!turns.length) return '<p class="hint">This conversation has no turns.</p>';
  let out = '';
  for (let i = 0; i < turns.length; i++) {
    const t = turns[i];
    if (t.role === 'tool_call') {
      // A call and the reply it got are one thing that happened, so they
      // are one block. Read as two rows they had to be paired up by eye,
      // and the verdict — the whole reason to open a conversation — sat on
      // the second of them.
      const next = turns[i + 1];
      const res = next && next.role === 'tool_result' ? turns[++i] : null;
      out += renderCommandTurn(t, res);
    } else if (t.role === 'tool_result') {
      // A result whose call was never recorded still shows, rather than
      // vanishing into a pairing rule.
      out += renderCommandTurn(null, t);
    } else {
      out += renderSpokenTurn(t);
    }
  }
  return '<div class="uses">' + out + '</div>';
}

// One thing said, by either side.
function renderSpokenTurn(t) {
  const who = t.speaker || (t.role === 'user' ? 'You' : 'Fono');
  const meta = [fmtWhen(t.ts)];
  if (t.latency_ms) meta.push(fmtMs(t.latency_ms));
  if (t.partial) meta.push('cut short');
  return '<div class="use turn ' + esc(t.role) + '">'
    + '<div class="who">' + esc(who)
    + '<span class="when">' + esc(meta.join(' \u00b7 ')) + '</span></div>'
    + '<div class="what">' + esc(t.text) + '</div></div>';
}

// One command the assistant sent, and what came back.
//
// Laid out exactly as #/actions lays out a tool's past uses, because it is
// the same question in a different place: which of these landed. Three
// states, not two — a call whose fate was never recorded must not be
// painted green, since "we did not check" and "it worked" are precisely the
// pair worth keeping apart. The arguments line is the fold: what was sent
// stays visible, because that is what catches a misrouted device name,
// while the server's answer is a click away for when the arguments look
// right and it still went wrong.
function renderCommandTurn(call, res) {
  const ok = res && typeof res.ok === 'boolean' ? res.ok : null;
  const cls = ok === true ? 'good' : ok === false ? 'bad' : '';
  const mark = ok === true ? '\u2713' : ok === false ? '\u2717' : '\u00b7';
  const word = ok === true ? 'worked' : ok === false ? 'failed' : 'not recorded';
  const verdict = ok === true ? 'This one landed.'
    : ok === false ? 'This one did not land.'
      : 'Nothing was recorded about how this one ended.';
  const text = ((call || res).text || '').trim();
  const cut = call ? text.indexOf(' ') : -1;
  const name = call ? (cut < 0 ? text : text.slice(0, cut)) : '';
  const args = call && cut >= 0 ? text.slice(cut + 1) : '';
  const meta = fmtWhen((call || res).ts) + ' \u00b7 <b class="'
    + (ok === false ? 'v-bad' : ok === true ? 'v-ok' : 'v-none') + '">' + esc(word) + '</b>';
  const sent = '<span class="m">' + mark + '</span>'
    + (name ? '<b>' + esc(name) + '</b>' : '<i>the call was not recorded</i>')
    + (args ? ' ' + esc(args) : '');
  const reply = res ? res.text : '';
  return '<div class="use turn command' + (cls ? ' ' + cls : '') + '" title="'
    + esc(verdict) + '">'
    + '<div class="who">Command<span class="when">' + meta + '</span></div>'
    + (reply
      ? '<details class="got"><summary class="sent mono" title="What came back.">'
      + sent + '</summary><div class="reply mono">' + esc(reply) + '</div></details>'
      : '<div class="sent mono">' + sent + '</div>')
    + '</div>';
}

function renderThreadList() {
  if (!histThreads) return '<p class="hint">Loading\u2026</p>';
  if (!histThreads.length) {
    return '<p class="hint">No conversations saved yet. They appear here after you talk to '
      + 'the assistant (unless you turned saving off).</p>';
  }
  return histThreads.map((t) => {
    const open = histOpen.has(t.id);
    const who = (t.speakers || []).map(speakerChip).join(' ');
    const bits = [esc(fmtWhen(t.last_at)),
      esc(t.turn_count + ' turn' + (t.turn_count === 1 ? '' : 's'))];
    // The model that answered, not the backend that carried the request:
    // "llama-local-assistant" is a name only Fono's own source explains,
    // and it is the same word whichever model is loaded. Older rows saved
    // before the model was recorded fall back to the backend, which is at
    // least true.
    if (t.model) {
      bits.push('<span' + (t.backend ? ' title="Answered through the '
        + esc(t.backend) + ' backend."' : '') + '>' + esc(t.model) + '</span>');
    } else if (t.backend) {
      bits.push(esc(t.backend));
    }
    // "Still open" said nothing a reader could act on. What it means is
    // that nothing has closed this conversation, so the newest one is the
    // one Fono carries on from when you speak again.
    if (!t.ended) {
      bits.push('<span title="Nothing has closed this conversation. If it is the '
        + 'newest one, speaking again carries on from here; otherwise Fono stopped '
        + 'before it could close it.">never closed</span>');
    }
    return '<div class="hthread">'
      + '<div class="row hrow"><div class="info">'
      + '<div class="lbl"><button class="linkbtn" type="button" data-hthread="' + t.id + '">'
      + (open ? '\u25bc ' : '\u25b6 ') + esc(t.preview || '(no user turn)') + '</button></div>'
      + '<div class="desc">' + bits.join(' \u00b7 ') + (who ? ' \u00b7 ' + who : '') + '</div>'
      + '</div><div class="ctl">'
      + '<button class="btn ghost" type="button" data-hcdel="' + t.id + '">Delete</button>'
      + '</div></div>'
      + (open ? '<div class="turns">' + renderTurns(histOpen.get(t.id)) + '</div>' : '')
      + '</div>';
  }).join('');
}

function renderHistory() {
  const el = document.getElementById('view-history');
  const tab = (id, label) => '<button class="btn' + (histTab === id ? ' primary' : ' ghost')
    + '" type="button" data-htab="' + id + '">' + label + '</button>';
  const bar = '<div class="doctor-bar">'
    + '<a class="btn ghost" href="#/settings">\u2190 Settings</a>'
    + tab('conversations', 'Conversations') + tab('dictation', 'Dictation')
    + '<span class="hint" style="margin-left:auto">' + (histBusy ? 'loading\u2026' : '') + '</span>'
    + '<button class="btn ghost" type="button" id="hclear">Clear all</button>'
    + '<button class="btn" type="button" id="hrefresh"' + (histBusy ? ' disabled' : '') + '>Refresh</button>'
    + '</div>';
  const search = histTab === 'dictation'
    ? '<div class="search"><span style="color:var(--ink-dim)">\u2315</span>'
      + '<input id="hq" placeholder="Search transcripts\u2026" autocomplete="off" value="'
      + esc(histQuery) + '" /></div>'
    : '';
  const body = histErr
    ? '<p class="privacy-note">Could not load history: ' + esc(histErr) + '</p>'
    : histTab === 'dictation' ? renderDictationList() : renderThreadList();
  el.innerHTML = bar + search
    + '<div class="hlist">' + body + '</div>'
    + '<p class="privacy-note" style="margin-top:20px;">Everything here is stored only on '
    + 'this machine. Retention and secret redaction are configured under History &amp; Privacy.</p>';

  el.querySelector('#hrefresh').addEventListener('click', () => {
    if (histTab === 'dictation') histDict = null; else { histThreads = null; histOpen.clear(); }
    loadHistory();
  });
  el.querySelector('#hclear').addEventListener('click', () => deleteHistory(histTab, null));
  el.querySelectorAll('[data-htab]').forEach((b) => b.addEventListener('click', () => {
    histTab = b.dataset.htab;
    renderHistory();
    if (histTab === 'dictation' ? histDict === null : histThreads === null) loadHistory();
  }));
  el.querySelectorAll('[data-hthread]').forEach((b) =>
    b.addEventListener('click', () => toggleThread(Number(b.dataset.hthread))));
  el.querySelectorAll('[data-hdel]').forEach((b) =>
    b.addEventListener('click', () => deleteHistory('dictation', Number(b.dataset.hdel))));
  el.querySelectorAll('[data-hcdel]').forEach((b) =>
    b.addEventListener('click', () => deleteHistory('conversations', Number(b.dataset.hcdel))));
  const q = el.querySelector('#hq');
  if (q) {
    q.addEventListener('input', (e) => {
      histQuery = e.target.value;
      clearTimeout(histSearchTimer);
      histSearchTimer = setTimeout(() => { histDict = null; loadHistory(); }, 250);
    });
    // Re-rendering replaces the node, so restore focus + caret.
    if (document.activeElement !== q && histQuery) {
      q.focus();
      q.setSelectionRange(histQuery.length, histQuery.length);
    }
  }
}

// ---------- tools & actions page (#/actions) ----------
// The list of everything the assistant can do, grouped by the server that
// offered it, with what each one expects and what Fono holds it to.
//
// Reads the same `/api/tools` payload the settings summary reads, which is
// built by the same code that builds the prompt. That is the point of the
// page: twice now a mechanism has worked correctly while the only place
// anyone could look was in another crate reporting something else, so the
// observation point is deliberately the same data, one hop from the model.
//
// Toggling writes immediately, like the settings section always has. Nothing
// here participates in Save/Discard — a page you visit mid-debug must not be
// able to strand an unsaved change somewhere else.
let actQuery = '';
const actOpen = new Set();     // "source\u0000tool" of the expanded rows
const actCollapsed = new Set(); // servers the user folded away
const sayJson = new Set();      // phrases whose exact command is showing
const actSecOpen = {};          // which folding sections the reader left open
let actBulkBusy = false;

const actKey = (src, name) => src + '\u0000' + name;
function fmtAgo(ts) {
  if (!ts) return '';
  const secs = Math.max(0, Math.floor(Date.now() / 1000 - ts));
  if (secs < 90) return 'just now';
  if (secs < 5400) return Math.round(secs / 60) + ' min ago';
  if (secs < 172800) return Math.round(secs / 3600) + ' h ago';
  return new Date(ts * 1000).toLocaleDateString();
}
// A tool's own first sentence, which is what the model reads when it decides
// which one to reach for. Shown as the row's title for exactly that reason:
// the row should say what the model was told, not what we would have written.
function toolTitle(t) {
  const d = (t.description || '').trim();
  if (!d) return t.name;
  const stop = d.search(/\.\s|\.$|\n/);
  return stop > 0 ? d.slice(0, stop) : d;
}
function pill(text, kind, title) {
  return '<span class="pill' + (kind ? ' ' + kind : '') + '"'
    + (title ? ' title="' + esc(title) + '"' : '') + '>' + esc(text) + '</span>';
}
// Badges appear only when something is *not* ordinary, and only when the row
// cannot already show it. Twenty-six rows each wearing "on", "available" and
// "reported" teaches nothing; one row wearing "missing" is the answer to why a
// command stopped working.
function toolPills(t) {
  let out = '';
  // No "off" badge: the whole row is already greyed out and its own switch is
  // sitting at the end of it, unticked. A third statement of the same fact only
  // competes with the badges that say something the row cannot show by itself.
  if (!t.available) {
    out += pill('missing', 'weak', 'The server has stopped offering this. Your choice is remembered in case it comes back.');
  }
  if (t.capability === 'dangerous') {
    out += pill('careful', 'weak', 'Its name suggests something hard to take back (unlock, delete, remove, reset, pay\u2026), so Fono will never run it by itself from a learned shortcut. Asking for it still works.');
  }
  if (t.verify_class === 'none') {
    out += pill('unverified', '', 'Nothing can be checked afterwards \u2014 Fono can only say the request was sent.');
  }
  return out;
}
// How a published field reads in plain language. Types and enums only: this
// describes the server's own schema and adds nothing to it.
function schemaType(p) {
  if (!p || typeof p !== 'object') return 'anything';
  if (Array.isArray(p.enum)) return 'one of ' + p.enum.length;
  const t = Array.isArray(p.type) ? p.type.filter((x) => x !== 'null')[0] : p.type;
  if (t === 'array') return 'a list of ' + schemaType(p.items || {});
  if (t === 'string') return 'text';
  if (t === 'integer' || t === 'number') {
    const lo = p.minimum, hi = p.maximum;
    return 'a number' + (lo !== undefined && hi !== undefined ? ' from ' + lo + ' to ' + hi : '');
  }
  if (t === 'boolean') return 'yes or no';
  if (t === 'object') return 'a structure';
  return t ? String(t) : 'anything';
}
// What Fono narrows a field to while the model writes a command — asked per
// server, because on a second server these are a different house.
//
// The three house-shaped field names are named by the vendor code daemon-side
// and arrive in `rails`, so a server Fono has no specific knowledge of shows
// nothing here rather than a guess.
function railsOf(source) {
  return (toolsData.rails || {})[source] || {};
}
// Whether a slot value can reach this field at all. The rails only narrow a
// field the server left as text — a field typed as a number is not silently
// turned into a word — and an array is narrowed item by item.
function narrowable(p) {
  const t = Array.isArray(p.type) ? p.type.filter((x) => x !== 'null')[0] : p.type;
  if (t === 'array') return narrowable(p.items && typeof p.items === 'object' ? p.items : {});
  return t === undefined || t === 'string';
}
// The one thing worth saying beside a field: how it *departs* from what the
// server card already said.
//
// Only two ways it can. The server published its own list, which beats anything
// Fono would supply; or the field is one of the three and the schema's type puts
// it out of reach, so nothing is held after all — worth a word, because that is
// the case where the sentence on the server card does not apply. A field held
// like the rest says nothing here: that sentence is true once, on the card,
// instead of twenty-three times down the page.
//
// The badge carries its own verb, so a new case cannot be bolted on with the
// wrong one.
function heldTo(field, p, source) {
  if (Array.isArray(p.enum)) return 'held to the ' + p.enum.length + ' the server listed';
  const r = railsOf(source);
  const slot = field === r.place || field === r.device || field === r.kind;
  if (slot && !narrowable(p)) {
    return 'not held to your home \u2014 the server wants ' + schemaType(p) + ' here';
  }
  return '';
}
// The sentence the field badges used to repeat on every row.
function railsSentence(source) {
  const r = railsOf(source), bits = [];
  const mono = (s) => '<span class="mono">' + esc(s) + '</span>';
  if (r.place && r.areas) bits.push(mono(r.place) + ' to your ' + r.areas + ' areas');
  if (r.device && r.devices) bits.push(mono(r.device) + ' to your ' + r.devices + ' devices');
  if (r.kind && r.kinds) {
    bits.push(mono(r.kind) + ' to the ' + r.kinds
      + ' kinds of device here, or everything in the area');
  }
  if (!bits.length) return '';
  return 'While a command is being written, Fono holds ' + bits.join(', ')
    + ' \u2014 so an area or a device this home does not have cannot be asked for.';
}
// How the last run ended, in the terms a person would use. Deliberately not
// the stored word: "accepted" and "sent" look interchangeable until you are
// told that only one of them was actually checked.
const RUN_OUTCOME = {
  confirmed: ['worked', 'Fono re-read the state afterwards and it had really changed.'],
  accepted: ['accepted', 'The server reported no problem, but nothing was re-read to confirm it.'],
  sent: ['sent', 'The request went out. Nothing about the result could be checked.'],
  failed: ['failed', 'It failed, or the check said nothing had changed.'],
};
// "Did this ever work, when, for whom, and how slow was it." The first
// question anyone opens a row to ask, so it sits at the top of the drawer.
// Every part is omitted when unknown rather than shown as a blank, because a
// row of dashes reads as broken.
function actHistory(t) {
  const r = t.last_run;
  if (!r) {
    return '<p class="act-hist none">Never used. Discovered and offered to the assistant, '
      + 'but no command has reached it yet.</p>';
  }
  const o = RUN_OUTCOME[r.outcome] || RUN_OUTCOME.sent;
  const bad = r.outcome === 'failed';
  // The verdict is the one word worth colouring. Painting the whole sentence
  // red made the timings and the speaker's name read as complaints too, and
  // whether it worked stopped being findable inside its own line.
  const bits = ['Last used ' + esc(fmtAgo(r.at))];
  bits.push('<b class="' + (bad ? 'v-bad' : 'v-ok') + '">it ' + esc(o[0]) + '</b>');
  // Two clocks, because they answer different questions and the smaller one
  // was being reported alone. The assistant deciding *which* command to send is
  // usually the larger half of the wait; the round trip to the house is the
  // half anyone can do something about. One number for both would have hidden
  // whichever of the two is actually the problem.
  if (typeof r.think_ms === 'number' && r.think_ms > 0) {
    bits.push('<span title="How long the assistant took to decide on this command, '
      + 'measured from the end of whatever it was doing before.">the assistant took '
      + esc(fmtMs(r.think_ms)) + ' to decide</span>');
  }
  if (typeof r.ms === 'number' && r.ms > 0) {
    bits.push('<span title="The round trip to the server, plus any re-reading Fono did '
      + 'afterwards to confirm it.">' + esc(fmtMs(r.ms)) + ' to carry out</span>');
  }
  if (r.speaker) bits.push('asked by ' + esc(r.speaker));
  if (t.runs > 1) bits.push(esc(t.runs + ' runs in all'));
  return '<p class="act-hist" title="' + esc(o[1]) + '">' + bits.join(' \u00b7 ') + '</p>';
}
// The same history as a fixed column on the right of the row, rather than more
// words trailing the tool's name.
//
// Two rules earn their keep here. It has to be a *column*: appended to a name,
// the status starts at a different x on every row, so twenty-six of them cannot
// be compared without reading each one, which is the opposite of what a
// debugging page is for. And it has to be *two facts*, not five — how it went,
// and when. Duration, who asked and the running total were on the row and made
// the one word that matters ("failed") indistinguishable from its own footnotes;
// they are a click away in the drawer, where someone has already chosen a row.
//
// Rows nothing has ever reached print nothing at all, so in a long catalogue the
// two or three that have run are the only marks in an otherwise empty strip.
function actRowHistory(t) {
  const r = t.last_run;
  if (!r) return '<span class="act-ran"></span>';
  const o = RUN_OUTCOME[r.outcome] || RUN_OUTCOME.sent;
  const bad = r.outcome === 'failed';
  const why = [o[1]];
  if (r.speaker) why.push('Asked by ' + r.speaker + '.');
  if (typeof r.think_ms === 'number' && r.think_ms > 0) {
    why.push('The assistant took ' + fmtMs(r.think_ms) + ' to decide on it.');
  }
  if (typeof r.ms === 'number' && r.ms > 0) why.push('Carrying it out took ' + fmtMs(r.ms) + '.');
  if (t.runs > 1) why.push('Run ' + t.runs + ' times in all.');
  why.push('Open the row for the rest.');
  return '<span class="act-ran' + (bad ? ' bad' : ' good') + '" title="'
    + esc(why.join(' ')) + '">'
    + '<span class="w">' + esc(o[0]) + '</span>'
    + '<span class="a">' + esc(fmtAgo(r.at)) + '</span></span>';
}
function fmtMs(ms) {
  return ms < 1000 ? ms + ' ms' : (ms / 1000).toFixed(ms < 10000 ? 1 : 0) + ' s';
}
// What this tool has actually been asked to do, in the user's own words.
//
// The most useful thing on the page, so it goes first. A schema tells you what
// a tool *could* be sent; this tells you what it *was* sent and what came back
// — which is where the bugs are. Reading one of these rows is how you see at a
// glance that "turn off the office light" became {"area":"Office light"}: a
// device name routed into the area field. That took a manual dump of the
// server's schemas and an afternoon of trace-reading to find the first time.
//
// Read back out of the ordinary conversation history, so it is here only while
// the user keeps one. Nothing is recorded for this page's benefit.
function actUses(t) {
  const uses = (toolsData.uses || {})[t.name] || [];
  if (!uses.length) {
    if (toolsData.history_kept === false) {
      return '<p class="hint">You keep no conversation history, so what this was asked to do '
        + 'is not recorded. Switch history on under <a href="#/settings">Conversations</a> to '
        + 'see the commands that reach it.</p>';
    }
    return '';
  }
  let out = '<div class="uses">';
  for (const u of uses) {
    // Three states, not two. A call whose fate was never recorded must not be
    // painted green: "we did not check" and "it worked" are exactly the pair
    // this page exists to keep apart.
    const cls = u.ok === true ? 'good' : u.ok === false ? 'bad' : '';
    const mark = u.ok === true ? '\u2713' : u.ok === false ? '\u2717' : '\u00b7';
    const verdict = u.ok === true ? 'This one landed.'
      : u.ok === false ? 'This one did not land.'
        : 'Nothing was recorded about how this one ended.';
    // The arguments line *is* the fold. A separate "what came back" row was one
    // more thing to read on every attempt, saying nothing until opened; the
    // request already names what the reply would be about, so it can carry the
    // reply itself. What was asked and what was sent stay visible, because that
    // pair is what catches a misrouted area name; the server's answer is a long
    // line of punctuation and is one click away, for when the arguments look
    // right and it still went wrong. Attempts with no recorded reply are a plain
    // line rather than a fold that opens onto nothing.
    const sent = '<span class="m">' + mark + '</span>' + esc(u.args || '(no arguments)');
    out += '<div class="use' + (cls ? ' ' + cls : '') + '" title="' + esc(verdict) + '">'
      + '<div class="said">' + (u.said ? '\u201c' + esc(u.said) + '\u201d'
        : '<i>no spoken command recorded</i>')
      + '<span class="when">' + esc(fmtAgo(u.at))
      + (u.speaker ? esc(' \u00b7 ' + u.speaker) : '') + '</span></div>'
      + (u.result
        ? '<details class="got"><summary class="sent mono" title="What the server said back.">'
        + sent + '</summary><div class="reply mono">' + esc(u.result) + '</div></details>'
        : '<div class="sent mono">' + sent + '</div>')
      + '</div>';
  }
  return out + '</div>';
}
// The published fields, one line each.
//
// Was a three-column table whose middle column read "text", "a list of text",
// "a list of one of 21" — true, and almost never the thing anyone came to find
// out. What matters is which fields exist, which are compulsory, and which ones
// Fono is narrowing; the type is a suffix on the same line. Fields Fono does not
// narrow simply say nothing rather than printing a dash, so the ones it does
// narrow are what the eye lands on.
function actFields(t) {
  const schema = t.schema && typeof t.schema === 'object' ? t.schema : {};
  const props = schema.properties && typeof schema.properties === 'object' ? schema.properties : {};
  const required = Array.isArray(schema.required) ? schema.required : [];
  const names = Object.keys(props);
  if (!names.length) {
    return '<p class="hint">The server publishes no fields for this, so the assistant sends it empty.</p>';
  }
  let out = '<ul class="act-fields">';
  for (const f of names) {
    const p = props[f] && typeof props[f] === 'object' ? props[f] : {};
    const listed = Array.isArray(p.enum) ? p.enum.join(', ') : '';
    const h = heldTo(f, p, t.source);
    out += '<li' + (listed ? ' title="' + esc(listed) + '"' : '') + '>'
      + '<span class="f mono">' + esc(f) + '</span>'
      + (required.includes(f) ? '<span class="req">must</span>' : '')
      + '<span class="ty">' + esc(schemaType(p)) + '</span>'
      + (h ? '<span class="held">' + esc(h) + '</span>' : '')
      + '</li>';
  }
  return out + '</ul>';
}
// The server's own words for this tool, exactly as published.
//
// Collapsed, because it is the answer to "is Fono reading this right?" rather
// than a thing to read every visit — but present, because every time that
// question has come up the answer has so far required dumping `tools/list` by
// hand from a terminal.
function actRaw(t) {
  let text;
  try {
    text = JSON.stringify({ name: t.name, description: t.description, inputSchema: t.schema }, null, 2);
  } catch (e) {
    text = String(t.schema);
  }
  return '<details class="act-raw"><summary>What the server published, word for word</summary>'
    + '<pre class="mono">' + esc(text) + '</pre></details>';
}
// `saidAtServer` is true when every tool here is verified the same way, so the
// server card already states it once and the row must not repeat it. When they
// differ, the row is the only place it can be said — dropping it there to save
// a line would hide the one thing this page exists to make visible.
function actDetail(t, saidAtServer) {
  const full = (t.description || '').trim();
  const title = toolTitle(t);
  // The row already shows the first sentence. Repeating it here, on every row,
  // was the single largest piece of duplication on the page.
  const rest = full.startsWith(title) ? full.slice(title.length).replace(/^[.\s]+/, '') : full;

  let out = '<div class="act-detail">';
  if (rest) out += '<p class="act-said">' + esc(rest) + '</p>';
  out += actHistory(t);
  out += actUses(t);
  out += actFields(t);
  if (!saidAtServer) {
    const p = TOOL_PROOF[t.verify_class] || TOOL_PROOF.none;
    out += '<p class="hint">Afterwards: <b>' + esc(p[0]) + '</b> \u2014 ' + esc(p[1])
      + (t.readback_tool
        ? ' Checked with <span class="mono">' + esc(t.readback_tool) + '</span>.' : '')
      + '</p>';
  }
  out += actRaw(t);
  return out + '</div>';
}
// The things that are true of every tool on a server, said once for the server
// instead of once per tool.
//
// On a stock Home Assistant this block was identical across all twenty-three
// rows — same verification story, same "no field is required", often the very
// same schema fingerprint, and the same three fields held to the same house.
// Twenty-three copies of a sentence do not inform anyone; they train the eye to
// skip the region where the differences live.
function actServerFacts(name, tools) {
  const bits = [];
  const proofs = new Set(tools.map((t) => t.verify_class || 'none'));
  if (proofs.size === 1) {
    const p = TOOL_PROOF[[...proofs][0]] || TOOL_PROOF.none;
    const readback = [...new Set(tools.map((t) => t.readback_tool).filter(Boolean))];
    bits.push('Afterwards, for everything here: <b>' + esc(p[0]) + '</b> \u2014 ' + esc(p[1])
      + (readback.length === 1
        ? ' Checked with <span class="mono">' + esc(readback[0]) + '</span>.' : ''));
  }
  const withFields = tools.filter((t) => {
    const s = t.schema && t.schema.properties;
    return s && Object.keys(s).length;
  });
  const noneRequired = withFields.length
    && withFields.every((t) => !(Array.isArray(t.schema.required) && t.schema.required.length));
  if (noneRequired) {
    bits.push('Not one of these declares a required field, so nothing but '
      + 'the rules Fono adds stops the deciding one being left out.');
  }
  const shapes = new Set(tools.map((t) => t.schema_hash || ''));
  if (shapes.size === 1 && tools.length > 1) {
    bits.push('All ' + tools.length + ' expect exactly the same fields \u2014 fingerprint '
      + '<span class="mono">' + esc((tools[0].schema_hash || '').slice(0, 12)) + '</span>. '
      + 'The assistant is choosing between them on their names alone.');
  }
  // Said once for the server, because it is one fact about this server and not
  // twenty-three facts about its tools.
  const rails = railsSentence(name);
  if (rails) bits.push(rails);
  if (!bits.length) return '';
  return '<p class="act-srv-facts">' + bits.join(' ') + '</p>';
}
function actServerCard(name, tools) {
  const srv = (toolsData.servers || []).find((s) => s.name === name) || {};
  const on = tools.filter((t) => t.enabled && t.available).length;
  const used = tools.filter((t) => t.last_run).length;
  const folded = actCollapsed.has(name);
  const many = (toolsData.servers || []).length > 1;
  const anyOn = tools.some((t) => t.enabled);

  let head = '<div class="act-srv-head">'
    + '<button class="act-fold" type="button" data-act-fold="' + esc(name) + '">'
    + (folded ? '\u25b6' : '\u25bc') + '</button>'
    + '<div class="info"><div class="lbl">' + esc(name || '(unnamed server)') + '</div>'
    + '<div class="desc">' + esc(tools.length + (tools.length === 1 ? ' thing' : ' things')
      + ', ' + on + ' in use'
      + (used ? ', ' + used + ' ever run' : ', none ever run'))
    + (srv.last_seen ? esc(' \u00b7 answered ' + fmtAgo(srv.last_seen)) : '')
    + (srv.url ? ' \u00b7 <span class="mono">' + esc(srv.url) + '</span>' : '')
    + '</div></div>'
    // Only the bisecting lever survives here. "All on" and "All off" were a
    // pair of loaded guns beside twenty-three individual switches: they read as
    // tidying-up, they wipe a set of deliberate choices in one click, and there
    // is nothing to undo them with. "Only this" is the one bulk act with a
    // question behind it — *is it this server?* — and it only appears when
    // there is more than one server to be wrong about.
    + '<div class="ctl">'
    + (many && anyOn ? '<button class="btn ghost" type="button" data-act-solo="' + esc(name) + '"' + (actBulkBusy ? ' disabled' : '') + ' title="Switch everything else off, so anything that still works came from here.">Only this</button>' : '')
    + '</div></div>';

  if (folded) return '<div class="act-srv">' + head + '</div>';
  const uniformProof = new Set(tools.map((t) => t.verify_class || 'none')).size === 1;
  const rows = tools.map((t) => {
    const k = actKey(t.source, t.name);
    const open = actOpen.has(k);
    return '<div class="act-row' + (t.enabled && t.available ? '' : ' act-dim')
      + (open ? ' act-open' : '') + '">'
      + '<button class="act-head" type="button" data-act-src="' + esc(t.source)
      + '" data-act-name="' + esc(t.name) + '" aria-expanded="' + open + '">'
      + '<span class="chev">' + (open ? '\u25bc' : '\u25b6') + '</span>'
      + '<span class="info"><span class="lbl">' + esc(toolTitle(t)) + '</span>'
      + '<span class="desc"><span class="mono">' + esc(t.name) + '</span> ' + toolPills(t)
      + '</span>'
      + '</span></button>'
      + actRowHistory(t)
      + '<div class="ctl"><input type="checkbox" class="toggle" data-tool-toggle="1" data-tool-src="'
      + esc(t.source) + '" data-tool-name="' + esc(t.name) + '"' + (t.enabled ? ' checked' : '')
      + ' title="Let the assistant use this" /></div>'
      + '</div>' + (open ? actDetail(t, uniformProof) : '');
  }).join('');
  return '<div class="act-srv">' + head + '<div class="act-rows">' + rows + '</div>'
    + actServerFacts(name, tools) + '</div>';
}
// What one device's own history says, on the chip itself.
//
// Per device rather than per command, because that is the unit people notice —
// "the office lamp never comes on" is what gets reported, and a per-tool count
// cannot answer it: a single instruction naming an area reaches six things and
// routinely fails on one of them. Only servers that name what they touched can
// fill this in, so a device with no history reads as plain rather than as zero.
function deviceChip(d) {
  const aliased = d.name.includes(',');
  const used = (d.runs || 0) > 0;
  const bad = used && d.last_ok === false;
  const cls = ['chip', used ? (bad ? 'chip-bad' : 'chip-ok') : '', aliased ? 'chip-note' : '']
    .filter(Boolean).join(' ');
  const why = [];
  if (used) {
    why.push((bad ? 'The last command to reach this did not land' : 'Last reached')
      + ' ' + fmtAgo(d.last_run) + '. '
      + (d.runs === 1 ? 'Reached once in all.' : 'Reached ' + d.runs + ' times in all.'));
  } else {
    why.push('No command has ever reached this one.');
  }
  if (aliased) why.push('Several names for one device. Fono asks for the first and recognises any of them.');
  return '<span class="' + cls + '" title="' + esc(why.join(' ')) + '">' + esc(d.name)
    + (used ? '<b class="n">' + esc(String(d.runs)) + '</b>' : '') + '</span>';
}
function actHousePanel() {
  const h = toolsData.house || {};
  const places = h.places || [], devices = h.devices || [], kinds = h.kinds || [];
  if (!places.length && !devices.length) return '';
  const chip = (s, cls, title) => '<span class="chip' + (cls ? ' ' + cls : '') + '"'
    + (title ? ' title="' + esc(title) + '"' : '') + '>' + esc(s) + '</span>';
  const byKind = new Map();
  for (const d of devices) {
    const k = d.domain || '';
    if (!byKind.has(k)) byKind.set(k, []);
    byKind.get(k).push(d);
  }
  // A name with a comma in it is a list of other names for one device. Fono
  // sends the first and matches on any of them — worth saying, because it is
  // invisible in the home's own screens and it decides which name works.
  const aliased = devices.filter((d) => d.name.includes(',')).length;
  const used = devices.filter((d) => (d.runs || 0) > 0);
  const failing = used.filter((d) => d.last_ok === false);
  let body = places.length
    ? '<div class="chips"><div class="chips-lbl">Areas</div>'
      + places.map((p) => chip(p)).join('') + '</div>'
    : '';
  for (const [k, list] of byKind) {
    body += '<div class="chips"><div class="chips-lbl' + (k ? ' mono' : '') + '">'
      + esc(k || 'kind not reported') + '</div>'
      + list.map(deviceChip).join('')
      + '</div>';
  }
  if (used.length) {
    body += '<p class="hint">The number on a device is how many times a command has actually '
      + 'reached it. Green worked last time'
      + (failing.length
        ? '; amber did not \u2014 ' + failing.map((d) => d.name.split(',')[0]).join(', ') + '.'
        : '.')
      + '</p>';
  } else if (devices.length) {
    body += '<p class="hint">No command has reached any of these yet. Once one does, the device '
      + 'itself carries the count \u2014 which is how you tell \u201cnothing works\u201d from '
      + '\u201cthis one thing never does\u201d.</p>';
  }
  if (aliased) {
    body += '<p class="hint">' + aliased + (aliased === 1 ? ' device has' : ' devices have')
      + ' more than one name, separated by commas. Fono asks for the first one and '
      + 'recognises the rest.</p>';
  }
  if (!kinds.length && devices.length) {
    body += '<p class="hint">This server did not say what kind each device is, so a command '
      + 'cannot be narrowed to \u201cjust the lights\u201d.</p>';
  }
  const sum = [places.length + (places.length === 1 ? ' area' : ' areas'),
    devices.length + (devices.length === 1 ? ' device' : ' devices'),
    used.length ? used.length + ' ever reached' : 'none ever reached'].join(' \u00b7 ');
  return '<details class="sec dsec" id="d-house"><summary><span class="chev">\u25b6</span>'
    + '<span class="t">What your home told Fono</span><span class="sum">' + esc(sum) + '</span>'
    + '</summary><div class="body">'
    + '<p class="hint">Learned when each server was connected, and re-read on every refresh '
    + '\u2014 so naming an area costs nothing while you are waiting. These names go to the '
    + 'assistant and nowhere else.</p>' + body + '</div></details>';
}
// One block of the system prompt, verbatim, with what it costs.
//
// Collapsible per block because the three are read for different reasons: the
// house list to check a name, the tool list to see what is on offer, the rules
// to see what the model was asked to do. Opening all three at once is what the
// old panel effectively did with one of them.
function promptBlock(label, note, text, open) {
  const n = (text || '').length;
  return '<details class="prompt-d"' + (open ? ' open' : '') + '><summary>'
    + '<span class="lbl">' + esc(label) + '</span>'
    + '<span class="hint">' + esc(note) + '</span>'
    + '<span style="margin-left:auto" class="hint">' + n.toLocaleString() + ' characters'
    + '</span></summary><pre class="act-prompt mono">' + esc(text) + '</pre></details>';
}
function actPromptPanel() {
  const p = toolsData.prompt || {};
  const hint = p.house || toolsData.hint;
  const n = toolsData.offered || 0;
  const total = p.chars || 0;
  const sum = total
    ? total.toLocaleString() + ' characters'
    : (hint ? hint.length.toLocaleString() + ' characters' : 'nothing about your home');
  let body = '<p class="hint">Every turn, the assistant reads these blocks in this order, '
    + 'and nothing else about your home reaches it. The last one is how to answer, not what '
    + 'to answer with \u2014 it is deliberately last, because a small model forgets an '
    + 'instruction sitting a thousand words back.</p>';
  if (hint) {
    body += promptBlock('Your home', 'areas, rules, devices', hint, true);
  } else {
    body += '<p class="hint">The assistant is told nothing about your areas or devices. '
      + 'Switch on <a href="#/settings">Tell the assistant your area names</a> so a command '
      + 'in another language can still find the right area.</p>';
  }
  if (p.tools) {
    body += promptBlock('What it can do',
      n + (n === 1 ? ' tool' : ' tools')
        + (p.tools_in_prompt ? '' : ' \u2014 sent as data, not as words'),
      p.tools, false);
  }
  if (p.behaviour) body += promptBlock('How to answer', 'your reply style', p.behaviour, false);
  if (p.tools && !p.tools_in_prompt) {
    body += '<p class="hint">This assistant is given its tools in the request itself rather '
      + 'than as words in the prompt, so that block costs the model nothing to read. A local '
      + 'model reads every character of it.</p>';
  }
  body += '<div class="enroll-row"><button class="btn" type="button" id="actcopy">Copy all'
    + '</button><span class="hint">Catalogue fingerprint <span class="mono">'
    + esc((toolsData.catalogue_hash || '').slice(0, 12))
    + '</span> \u2014 changes exactly when these instructions would.</span></div>';
  return '<details class="sec dsec" id="d-prompt"><summary><span class="chev">\u25b6</span>'
    + '<span class="t">The exact words the assistant is given</span>'
    + '<span class="sum">' + esc(sum) + '</span></summary><div class="body">' + body + '</div></details>';
}
// The phrases Fono has written down, and which of them it can run on its own.
//
// Above the servers because it is the one section about *you* rather than about
// them: what you actually say, and whether saying it still costs a round trip
// through the assistant. The list of phrases that have worked but never earned
// the fast path is the assistant's own blind-spot list, so it is shown rather
// than hidden — those are the sentences worth turning into test cases.
function actPhrasesPanel() {
  const rows = toolsData.shortcuts || [];
  const fast = rows.filter((s) => s.state === 'fast').length;
  const sum = rows.length
    ? rows.length + (rows.length === 1 ? ' phrase' : ' phrases') + ' \u00b7 '
      + (fast ? fast + ' run without the assistant' : 'none earned yet')
    : 'nothing yet';
  let body = '<p class="hint">Say something that works twice and Fono writes it down. From then '
    + 'on it runs the same command the moment it hears the phrase \u2014 the assistant is not '
    + 'asked at all, so there is nothing to wait for. No command here was typed in: a phrase '
    + 'earns its place by working. Give one that has earned it another wording with '
    + '<b>+</b> and that wording works straight away.</p>';
  if (!rows.length) {
    body += '<p class="hint">No phrase has earned this yet. It takes the same words twice, '
      + 'each time with no error and no correction from you.</p>';
  } else {
    body += '<div class="act-rows">' + phraseGroups(rows).map(phraseGroup).join('') + '</div>';
  }
  return '<details class="sec dsec" id="d-say"><summary><span class="chev">\u25b6</span>'
    + '<span class="t">Things you can say</span><span class="sum">' + esc(sum) + '</span>'
    + '</summary><div class="body">' + body + '</div></details>';
}
// Every wording of one command, gathered under the wording that earned it.
//
// Grouped by the command rather than by which row was typed in, because two
// phrases that send the identical thing to the identical server *are* one entry
// in a person's head. Ungrouped, an added wording sorted to the far end of the
// list by its own empty record, which is the last place anyone would look for
// it. The head of a group is the wording Fono heard work, most-said first; the
// rest hang under it and can be removed one at a time.
function phraseGroups(rows) {
  const by = new Map();
  for (const s of rows) {
    const k = JSON.stringify([s.source, s.tool, s.args]);
    if (!by.has(k)) by.set(k, []);
    by.get(k).push(s);
  }
  const heard = (s) => (s.origin === 'written' ? 1 : 0);
  return [...by.values()].map((g) => g.slice().sort((a, b) =>
    heard(a) - heard(b) || (b.runs || 0) - (a.runs || 0) || (b.last_run || 0) - (a.last_run || 0)));
}
function phraseGroup(g) {
  const head = g[0];
  return '<div class="say-group">' + phraseRow(head) + g.slice(1).map(aliasRow).join('')
    + (sayJson.has(head.phrase) ? sayJsonBlock(head) : '') + '</div>';
}
// One state word per phrase, never two \u2014 and the row's own record beside it.
//
// Only a state that wants something from you is coloured. "learning" is the
// ordinary life of a phrase that is working perfectly well, and colouring it
// made a page of healthy rows read as a page of faults.
const SAY_STATE = {
  fast: ['fast', 'good', 'Fono runs this the moment it hears it. The assistant is not asked.'],
  learning: ['learning', '', 'This has worked. One more clean run and Fono will stop asking the assistant about it.'],
  written: ['yours', '', 'You added this wording. It runs as readily as the phrase you copied it from \u2014 the command is the same one, already proven.'],
  paused: ['paused', 'weak', 'The command behind it is switched off, or its server has stopped offering it. It resumes when the tool does \u2014 nothing has to be learned again.'],
  changed: ['changed', 'weak', 'The command has changed shape since this was written down, so replaying it would no longer be what worked. Say it again and Fono will learn the new shape.'],
};
// Three small square buttons, one glyph each. Text buttons for "Another way"
// and "Forget" took more width than the phrase they belonged to and shouted for
// attention the phrase deserved. Written out one by one rather than built from
// an argument, so the check that every button has a handler can still read them.
function sayAlsoBtn(p) {
  const t = 'Add another way of saying this. It runs the same command.';
  return '<button class="ibtn" type="button" data-say-also="' + esc(p)
    + '" title="' + t + '" aria-label="' + t + '">+</button>';
}
function sayJsonBtn(p) {
  const t = (sayJson.has(p) ? 'Hide' : 'Show') + ' exactly what this sends';
  return '<button class="ibtn" type="button" data-say-json="' + esc(p)
    + '" title="' + t + '" aria-label="' + t + '">{ }</button>';
}
function sayForgetBtn(p, t) {
  return '<button class="ibtn" type="button" data-say-forget="' + esc(p)
    + '" title="' + t + '" aria-label="' + t + '">\u00d7</button>';
}
// Exactly what would be sent, on request. Every row now reads as prose \u2014 the
// tool and the thing it touches \u2014 so the arguments no longer leak into rows
// whose server does not name its fields, and are one click away for all of them.
function sayJsonBlock(s) {
  let args = s.args;
  try { args = JSON.stringify(JSON.parse(s.args), null, 2); } catch (err) { /* as stored */ }
  return '<pre class="say-json mono">' + esc(s.source + ' \u00b7 ' + s.tool + '\n' + args) + '</pre>';
}
function phraseRow(s) {
  const st = SAY_STATE[s.state] || SAY_STATE.learning;
  // Two situations wear the same word: a run whose half-minute is still open,
  // and one the user contradicted. Both count nothing yet, and saying "one more
  // run" about either would be wrong \u2014 so say what a run has to do.
  const tip = s.state === 'learning' && !s.clean
    ? 'This has worked, but no run counts yet. A run counts once half a minute goes by '
      + 'without you saying the same thing again \u2014 asking twice straight away reads as '
      + '\u201cthat was wrong\u201d. Two counted runs earn the fast path.'
    : st[2];
  // What it does is the footnote; what you say is the row. The thing in the
  // house is pulled out when the server names its fields; when it does not, the
  // tool stands alone rather than the row turning into a line of JSON.
  const does = s.tool + (s.target ? ' \u00b7 ' + s.target : '');
  const why = [tip];
  why.push(s.runs === 1 ? 'Said once.' : 'Said ' + s.runs + ' times.');
  if (typeof s.last_ms === 'number' && s.last_ms > 0) {
    why.push('Last time the command itself took ' + fmtMs(s.last_ms) + '.');
  }
  const word = s.last_ok === true ? 'worked' : s.last_ok === false ? 'did not' : 'never run';
  const cls = s.last_ok === true ? ' good' : s.last_ok === false ? ' bad' : '';
  return '<div class="act-row act-say' + (s.state === 'paused' ? ' act-dim' : '') + '">'
    + '<span class="info"><span class="lbl">\u201c' + esc(s.phrase) + '\u201d</span>'
    + '<span class="desc"><span class="mono">' + esc(does) + '</span> '
    + pill(st[0], st[1], tip) + '</span></span>'
    + '<span class="act-ran' + cls + '" title="' + esc(why.join(' ')) + '">'
    + '<span class="w">' + esc(word) + '</span>'
    + '<span class="a">' + esc(s.last_run ? fmtAgo(s.last_run) : '') + '</span></span>'
    + '<div class="ctl">'
    + sayAlsoBtn(s.phrase) + sayJsonBtn(s.phrase)
    + sayForgetBtn(s.phrase, 'Forget this phrase')
    + '</div></div>';
}
// Another wording of the same command, indented under the one above it. Some
// are copies the user made and some are simply a second thing Fono heard work;
// either way it is the same command said differently rather than a second thing
// Fono knows, so it carries only what differs — the words, how it is faring,
// and when it last ran.
function aliasRow(s) {
  const st = SAY_STATE[s.state] || SAY_STATE.written;
  const why = s.runs === 1 ? 'Said once.' : 'Said ' + s.runs + ' times.';
  return '<div class="say-alias' + (s.state === 'paused' ? ' act-dim' : '') + '">'
    + '<span class="arr">\u21b3</span>'
    + '<span class="lbl">\u201c' + esc(s.phrase) + '\u201d</span>'
    + pill(st[0], st[1], st[2])
    + '<span class="ago" title="' + esc(why) + '">'
    + esc(s.last_run ? fmtAgo(s.last_run) : 'never run') + '</span>'
    + sayForgetBtn(s.phrase, 'Forget this wording. The others stay.')
    + '</div>';
}
// Editing which command a phrase runs is deliberately not offered: that mapping
// is won by working twice, and letting it be typed in would make the winning
// decorative. Adding a wording and forgetting one are the two edits that cannot
// lie about what has been verified.
async function addPhrase(like) {
  const said = prompt('Another way to say \u201c' + like + '\u201d:', '');
  if (!said || !said.trim()) return;
  try {
    await api('/api/shortcuts', { method: 'POST', body: JSON.stringify({ like, phrase: said.trim() }) });
    toast('Added');
    await loadTools();
    renderActions();
  } catch (err) { toast('Could not add that: ' + err.message, true); }
}
// One button forgets one wording, never its neighbours. A group holds wordings
// the user copied *and* wordings Fono heard work in their own right, and there
// is no way to tell from a row which it is — so taking the others down with it
// would sometimes throw away something earned.
async function forgetPhrase(phrase) {
  if (!confirm('Forget \u201c' + phrase + '\u201d? Fono will ask the assistant again next time.')) return;
  try {
    await api('/api/shortcuts?phrase=' + encodeURIComponent(phrase), { method: 'DELETE' });
    sayJson.delete(phrase);
    toast('Forgotten');
    await loadTools();
    renderActions();
  } catch (err) { toast('Could not forget that: ' + err.message, true); }
}
function actBody() {
  if (toolsErr) return '<p class="privacy-note">Could not load what the assistant can do: ' + esc(toolsErr) + '</p>';
  if (!toolsData) return '<p class="hint">Loading\u2026</p>';
  const all = toolsData.tools || [];
  const servers = toolsData.servers || [];
  if (!all.length) {
    return '<p class="hint">' + (servers.length
      ? 'None of your servers has reported anything it can do yet. Press Refresh, or check the token under <a href="#/settings">Tools &amp; actions</a>.'
      : 'No servers yet. Add one under <a href="#/settings">Tools &amp; actions</a> and Fono will ask it what it can do.') + '</p>';
  }
  const q = actQuery.trim().toLowerCase();
  const hit = (t) => !q || (t.name + ' ' + (t.description || '') + ' ' + t.source).toLowerCase().includes(q);
  const shown = all.filter(hit);

  const on = all.filter((t) => t.enabled && t.available).length;
  let out = '';
  if (toolsData.enabled === false) {
    out += '<p class="privacy-note">Letting the assistant control things is switched off, so '
      + 'none of this is offered to it. Turn it on under <a href="#/settings">Tools &amp; actions</a>.</p>';
  }
  const everRan = all.filter((t) => t.last_run).length;
  out += '<p class="act-lede">The assistant can do <b>' + on + '</b> of these '
    + esc(all.length + ' things, from ' + srvCount(servers.length)) + '.'
    + (everRan
      ? ' A real command has reached <b>' + everRan + '</b> of them \u2014 those rows say when '
        + 'and how it went.'
      : ' None of them has been used yet.')
    + ' While it writes a command it can only name areas and devices you actually have.'
    + ' <b>Open any row</b> to see the commands that have actually reached it \u2014 what was '
    + 'said, what was sent, and what came back.'
    + '</p>';
  out += '<div class="search"><span style="color:var(--ink-dim)">\u2315</span>'
    + '<input id="actq" placeholder="Filter what it can do\u2026" autocomplete="off" value="'
    + esc(actQuery) + '" /><span class="kbd">/</span></div>';
  out += actPhrasesPanel();

  if (!shown.length) {
    out += '<p class="hint">Nothing matches \u201c' + esc(actQuery) + '\u201d.</p>';
  } else {
    // Grouped by the server that offered it, because that is the unit a
    // person switches on and off, and the unit that breaks.
    const order = [];
    const byServer = new Map();
    for (const t of shown) {
      if (!byServer.has(t.source)) { byServer.set(t.source, []); order.push(t.source); }
      byServer.get(t.source).push(t);
    }
    out += order.map((s) => actServerCard(s, byServer.get(s))).join('');
  }
  return out + actHousePanel() + actPromptPanel();
}
function renderActions() {
  const el = document.getElementById('view-actions');
  // Every button on this page redraws the whole of it, and a redrawn <details>
  // has forgotten it was open. Remember it across the redraw: asking to see one
  // phrase's exact command used to fold the entire section away under the
  // reader, which is worse than not offering the button at all.
  el.querySelectorAll('details.sec').forEach((d) => { actSecOpen[d.id] = d.open; });
  const bar = '<div class="doctor-bar">'
    + '<a class="btn ghost" href="#/settings">\u2190 Settings</a>'
    + '<span class="hint" style="margin-left:auto">'
    + (toolsBusy ? 'asking your servers\u2026' : actBulkBusy ? 'saving\u2026' : '') + '</span>'
    + '<button class="btn" type="button" id="actrefresh"' + (toolsBusy ? ' disabled' : '') + '>Refresh</button>'
    + '</div>';
  el.innerHTML = bar + actBody();
  el.querySelectorAll('details.sec').forEach((d) => { d.open = !!actSecOpen[d.id]; });

  el.querySelector('#actrefresh').addEventListener('click', discoverTools);
  // Keyed by two attributes rather than one, because the key contains a NUL
  // separator and an HTML attribute cannot carry one: the parser silently
  // rewrites U+0000 to U+FFFD, so the key read back off a click never equalled
  // the key the renderer looks up, and no row would open. Building it here from
  // its two halves keeps the separator out of the document entirely.
  el.querySelectorAll('[data-act-name]').forEach((b) => b.addEventListener('click', () => {
    const k = actKey(b.dataset.actSrc, b.dataset.actName);
    if (actOpen.has(k)) actOpen.delete(k); else actOpen.add(k);
    renderActions();
  }));
  el.querySelectorAll('[data-act-fold]').forEach((b) => b.addEventListener('click', () => {
    const s = b.dataset.actFold;
    if (actCollapsed.has(s)) actCollapsed.delete(s); else actCollapsed.add(s);
    renderActions();
  }));
  el.querySelectorAll('[data-act-solo]').forEach((b) =>
    b.addEventListener('click', () => setServerTools(b.dataset.actSolo, true, true)));
  el.querySelectorAll('[data-say-also]').forEach((b) =>
    b.addEventListener('click', () => addPhrase(b.dataset.sayAlso)));
  el.querySelectorAll('[data-say-json]').forEach((b) => b.addEventListener('click', () => {
    const p = b.dataset.sayJson;
    if (sayJson.has(p)) sayJson.delete(p); else sayJson.add(p);
    renderActions();
  }));
  el.querySelectorAll('[data-say-forget]').forEach((b) =>
    b.addEventListener('click', () => forgetPhrase(b.dataset.sayForget)));
  const copy = el.querySelector('#actcopy');
  if (copy) copy.addEventListener('click', async () => {
    // The whole head, joined exactly as the daemon joins it, so what lands on
    // the clipboard is what the model was sent and can be diffed against a
    // trace.
    const p = toolsData.prompt || {};
    const text = [p.house || toolsData.hint, p.tools, p.behaviour]
      .filter((b) => b && b.trim()).join('\n\n');
    try {
      await navigator.clipboard.writeText(text);
      toast('Copied');
    } catch (err) { toast('Could not copy: ' + err.message, true); }
  });
  const q = el.querySelector('#actq');
  if (q) {
    q.addEventListener('input', (e) => { actQuery = e.target.value; renderActions(); });
    if (actQuery) { q.focus(); q.setSelectionRange(actQuery.length, actQuery.length); }
  }
}
// Switch a whole server on or off in one go, optionally silencing every other
// server as well. Bisecting a misrouted command is the point: with one server
// left, anything that still works came from there. Twenty-six separate saves
// cost one re-warm, not twenty-six \u2014 the daemon lets a burst settle first.
async function setServerTools(server, enabled, solo) {
  if (actBulkBusy) return;
  const wanted = (toolsData.tools || []).filter((t) =>
    t.source === server ? t.enabled !== enabled : solo && t.enabled)
    .map((t) => ({ source: t.source, name: t.name, enabled: t.source === server ? enabled : false }));
  if (!wanted.length) return;
  actBulkBusy = true;
  renderActions();
  try {
    for (const w of wanted) {
      await api('/api/tools', { method: 'PATCH', body: JSON.stringify(w) });
    }
  } catch (err) {
    toast('Could not change those: ' + err.message, true);
  }
  actBulkBusy = false;
  await loadTools();
  renderActions();
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
    loadTools(); // fire-and-forget: fills the Tools section
  } catch (err) {
    document.getElementById('loading').textContent = 'Could not load configuration: ' + err.message
      + (TOKEN ? '' : ' \u2014 if a token is configured, open this page as /?token=\u2026');
  }
}
init();
