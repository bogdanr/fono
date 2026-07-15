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
const OVERLAY_STYLES = [
  ['bars', 'Bars', 'p-bars', ''], ['oscilloscope', 'Oscilloscope', 'p-osc', ''],
  ['fft', 'FFT', 'p-fft', ''], ['heatmap', 'Heatmap', 'p-heat', ''],
  ['terrain3d', '3D Terrain', 'p-terr', ''], ['system360', 'System/360', 'p-dots', ''],
  ['cortex', 'Glass Cortex', 'p-terr', 'LLM brain view'],
  ['transcript', 'Transcript', 'p-text', 'more CPU/API'],
];
function pname(list, id) { const p = list.find((x) => x[0] === id); return p ? p[1] : id; }
function pdef(list, id) { const p = list.find((x) => x[0] === id); return p ? p[2] : ''; }

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
  return '<div class="pgrid ovgrid">' + OVERLAY_STYLES.map((s) =>
    '<button type="button" class="pcard ov" data-pick="overlay-style" data-val="' + s[0] + '" aria-pressed="' + (s[0] === cur) + '">'
    + '<div class="ovprev ' + s[2] + '"></div><div class="pname">' + esc(s[1]) + '</div>'
    + (s[3] ? '<div class="pmeta">' + esc(s[3]) + '</div>' : '') + '</button>').join('') + '</div>';
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
};

// Provider-card click handlers. The explicit `.provider` / `.api_key_ref`
// sets duplicate ensureCloud's work on purpose: the coverage test in
// web_settings/mod.rs greps this file for full dotted paths.
const PICK = {
  'stt-provider'(v) { ensureSttCloud(v); set(cfg, 'stt.backend', v); },
  'polish-provider'(v) {
    set(cfg, 'polish.backend', v);
    if (v !== 'local') {
      ensureCloud('polish.cloud', v);
      set(cfg, 'polish.cloud.provider', v);
      set(cfg, 'polish.cloud.api_key_ref', ENV[v] || '');
    }
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
      const b = gv('polish.backend', 'local');
      return b === 'local' ? 'Local model' : pname(POLISH_PROVIDERS, b);
    },
    html() {
      const on = gv('polish.enabled', false);
      const b = gv('polish.backend', 'local');
      const cloudBits = b !== 'local' && b !== 'none'
        ? '<div style="margin-top:12px">'
        + row('Model', 'Empty = provider default.', txt('polish.cloud.model', { mono: true, w: 220, ph: pdef(POLISH_PROVIDERS, b) }))
        + keyRow(ENV[b]) + '</div>'
        : '';
      return row('Enable cleanup', 'Runs each transcript through a small language model \u2014 punctuation, casing, filler removal.',
        toggle('polish.enabled', false, 'cleanup'), 'master')
        + '<div' + (on ? '' : ' class="section-off"') + '>'
        + '<div class="subhead">Provider</div>'
        + pgrid(POLISH_PROVIDERS, 'polish-provider', b === 'none' ? 'local' : b)
        + cloudBits
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
      const b = gv('assistant.backend', 'none');
      let s = b === 'none' ? 'no backend' : pname(ASSISTANT_PROVIDERS, b);
      if (gv('assistant.realtime.live_mode', true)) s += ' \u00b7 live mode on';
      return s;
    },
    html() {
      const on = gv('assistant.enabled', false);
      const b = gv('assistant.backend', 'none');
      const cloudBits = b !== 'none'
        ? '<div style="margin-top:12px">'
        + row('Model', 'Empty = provider default.', txt('assistant.cloud.model', { mono: true, w: 220, ph: pdef(ASSISTANT_PROVIDERS, b) }))
        + keyRow(ENV[b]) + '</div>'
        : '';
      return row('Enable assistant', 'Voice Q&A \u2014 ask a question, hear or read the answer.',
        toggle('assistant.enabled', false, 'assistant'), 'master')
        + '<div' + (on ? '' : ' class="section-off"') + '>'
        + '<div class="subhead">Provider</div>'
        + pgrid(ASSISTANT_PROVIDERS, 'assistant-provider', b)
        + cloudBits
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
        panel = row('Voice', 'Catalog voice id, e.g. en_US-lessac-medium. Empty = match your first language.',
          txt('tts.local.voice', { mono: true, w: 220, ph: 'auto' }));
      } else if (s === 'cloud') {
        const p = gv('tts.backend', 'openai');
        panel = '<div class="subhead">Provider</div>' + pgrid(TTS_PROVIDERS, 'tts-provider', p)
          + '<div style="margin-top:12px">'
          + row('Model', 'Empty = provider default.', txt('tts.cloud.model', { mono: true, w: 200, ph: pdef(TTS_PROVIDERS, p) }))
          + row('Voice', 'Voice id \u2014 see `fono voices`. Empty = backend default.', txt('tts.voice', { mono: true, w: 200, ph: 'default' }))
          + keyRow(ENV[p]) + '</div>';
      } else if (s === 'wyoming') {
        panel = row('Server URI', 'Wyoming protocol \u2014 e.g. tcp://10.0.0.4:10200.', txt('tts.wyoming.uri', { mono: true, w: 240 }))
          + row('Token ref', 'Optional pre-shared token reference.', txt('tts.wyoming.auth_token_ref', { mono: true, w: 180, ph: 'none' }))
          + row('Voice', 'Empty = server default.', txt('tts.voice', { mono: true, w: 200, ph: 'default' }));
      }
      const dev = s === 'none' ? '' : row('Output device', 'Empty = system default.', txt('tts.output_device', { w: 200, ph: 'System default' }));
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
        + srvCard('LLM API server (OpenAI + Ollama compatible)',
          srvField('Bind', srvInput('server.llm.bind', '127.0.0.1'))
          + srvField('Port', srvNum('server.llm.port', 11434))
          + srvField('Token ref', srvInput('server.llm.auth_token_ref', '', 'none'))
          + srvField('Model override', srvInput('server.llm.model', '', '\u2014')),
          'server.llm.enabled')
        + srvCard('Web settings (this page \u2014 changes apply after restart)',
          srvField('Bind', srvInput('server.web.bind', '127.0.0.1'))
          + srvField('Port', srvNum('server.web.port', 10808))
          + srvField('Token ref', srvInput('server.web.auth_token_ref', '', 'none')),
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
  if (!el.dataset || !el.dataset.bind) return;
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
  // Live sensitivity readout next to wake sliders.
  if (el.classList.contains('slider') && el.previousElementSibling && el.previousElementSibling.classList.contains('sens')) {
    el.previousElementSibling.textContent = Number(el.value).toFixed(2);
  }
});

document.addEventListener('click', (e) => {
  const t = e.target.closest('[data-seg],[data-pick],[data-tag-rm],[data-wake-rm],[data-wake-add],[data-vocab-rm],[data-vocab-add],[data-keycap],[data-reset],[data-key-edit],[data-key-clear],[data-key-save],[data-key-cancel]');
  if (!t) return;
  const secEl = t.closest('details.sec');
  const sec = secEl ? FONO_SECTIONS.find((s) => 'd-' + s.id === secEl.id) : null;

  if (t.dataset.seg) { SEG[t.dataset.seg](t.dataset.val); afterChange(t, sec); return; }
  if (t.dataset.pick) { PICK[t.dataset.pick](t.dataset.val); afterChange(t, sec); return; }
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
  } catch (err) {
    document.getElementById('loading').textContent = 'Could not load configuration: ' + err.message
      + (TOKEN ? '' : ' \u2014 if a token is configured, open this page as /?token=\u2026');
  }
}
init();
