// SPDX-License-Identifier: GPL-3.0-only
//
// Fono website OSCILLOSCOPE demo — combined dictation + assistant.
//
// Self-contained IIFE that animates a 600x300 canvas to simulate the
// Fono overlay's `WaveformStyle::Oscilloscope` rendering across both
// product modes (dictation + assistant Q&A). Mounts onto:
//   #fono-demo            (wrapper)
//   #fono-demo-canvas     (canvas)
//   .play-pill inside #fono-demo (reduced-motion play button)
//
// Algorithms mirrored from the real product:
//   * crates/fono/src/session.rs:49-80        — sample pipeline
//   * crates/fono/src/session.rs:941-977      — thinking standing-wave
//   * crates/fono-overlay/src/real.rs:53      — 5000-sample buffer
//   * crates/fono-overlay/src/real.rs:218-287 — panel palette
//   * crates/fono-overlay/src/real.rs:669-734 — draw_oscilloscope
//   * crates/fono-overlay/src/real.rs:1715-1735 — headroom selection

(() => {
  'use strict';

  // ── Constants mirrored from the Rust source ─────────────────────────────
  const SAMPLE_RATE     = 16000;
  const PRODUCER_SPEED  = 0.6;
  const OSC_BUFFER      = 2200;
  const PADDING_X       = 24;
  const PADDING_TOP     = 14;
  const PADDING_BOT     = 16;
  const ACCENT_WIDTH    = 4;
  const CORNER_RADIUS   = 12;
  const STATUS_FONT_PX  = 13;
  const STATUS_TO_TEXT  = 14;
  const TEXT_FONT_PX    = 20;
  const LINE_GAP        = 6;
  const COLOR_BG        = 'rgba(23, 23, 27, 0.80)';
  const COLOR_TEXT      = '#ECECF1';
  const COLOR_TEXT_DIM  = '#AAAAB2';
  const ACCENT_RECORDING = [0xE0, 0x54, 0x54];
  const ACCENT_POLISHING = [0xE0, 0xA0, 0x40];
  const ACCENT_ASSISTANT = [0x22, 0xC5, 0x5E];
  const ACCENT_THINKING  = [0xF5, 0x9E, 0x0B];
  const OSC_HEADROOM_REC   = 0.88;
  const OSC_HEADROOM_THINK = 1.00;

  // ── Canvas / sizing ─────────────────────────────────────────────────────
  const wrap   = document.getElementById('fono-demo');
  if (!wrap) return;
  const canvas = document.getElementById('fono-demo-canvas');
  const pill   = wrap.querySelector('.play-pill');
  const ctx    = canvas.getContext('2d');
  const W_CSS = 600, H_CSS = 300;
  function resizeBacking() {
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width  = W_CSS * dpr;
    canvas.height = H_CSS * dpr;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }
  resizeBacking();
  window.addEventListener('resize', resizeBacking);

  // ── Seeded PRNG ─────────────────────────────────────────────────────────
  function mulberry32(seed) {
    let s = seed >>> 0;
    return () => {
      s = (s + 0x6D2B79F5) >>> 0;
      let t = s;
      t = Math.imul(t ^ (t >>> 15), t | 1);
      t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }
  const rng = mulberry32(0x05C111E7 >>> 0);

  // ── Synthetic voice generator ───────────────────────────────────────────
  const FORMANT_SETS = [
    [ 730, 1090, 2440], [ 530, 1840, 2480], [ 270, 2290, 3010],
    [ 570,  840, 2410], [ 300,  870, 2240], [ 500, 1500, 2500],
  ];
  const N_HARMONICS   = 10;
  const ROLLOFF_EXP   = 3.2;
  const FORMANT_FLOOR = 0.6;
  const SIGNAL_GAIN   = 1.5;
  const NOISE_AMP     = 0.3;

  let noiseState = 0;
  function noise() {
    const w = (rng() * 2 - 1);
    noiseState = noiseState * 0.92 + w * 0.08;
    return noiseState;
  }

  const WORD_HZ          = 4.0;
  const WORD_ATTACK_F    = 0.04;
  const WORD_SUSTAIN_F   = 0.55;
  const WORD_DECAY_F     = 0.72;
  function envelope(tSec) {
    const wordIdx = Math.floor(tSec * WORD_HZ);
    const within  = tSec * WORD_HZ - wordIdx;
    if ((wordIdx % 4) === 2) return 0.0;
    if (within < WORD_ATTACK_F)  return within / WORD_ATTACK_F;
    if (within < WORD_SUSTAIN_F) return 1.0;
    if (within < WORD_DECAY_F) {
      return 1.0 - (within - WORD_SUSTAIN_F)
                     / (WORD_DECAY_F - WORD_SUSTAIN_F);
    }
    return 0.0;
  }

  function hash01(seed) {
    let x = (seed | 0) ^ 0x9E3779B9;
    x = Math.imul(x ^ (x >>> 16), 0x85EBCA6B);
    x = Math.imul(x ^ (x >>> 13), 0xC2B2AE35);
    x ^= x >>> 16;
    return ((x >>> 0) % 1000003) / 1000003;
  }
  function syllableParams(sylIdx) {
    const a = hash01(sylIdx * 73856093);
    const b = hash01(sylIdx * 19349663 + 1);
    return {
      f0: 130 + a * 40,
      fset: FORMANT_SETS[Math.floor(b * FORMANT_SETS.length)],
    };
  }
  function wordParams(wordIdx) { return syllableParams(wordIdx); }
  function formantGain(freq, F, BW) {
    const d = (freq - F) / BW;
    return 1 / (1 + d * d);
  }

  const oscBuf = new Float32Array(OSC_BUFFER);
  let   oscPos = 0;
  let   sampleClock = 0;

  function generateSamples(n) {
    for (let i = 0; i < n; i++) {
      const t = sampleClock / SAMPLE_RATE;
      const env = envelope(t);
      let s = 0;
      if (env > 0.03) {
        const wordIdx = Math.floor(t * WORD_HZ);
        const p = wordParams(wordIdx);
        const vibrato = 1 + 0.012 * Math.sin(2 * Math.PI * 5.5 * t);
        const f0 = p.f0 * vibrato;
        const F = p.fset;
        for (let k = 1; k <= N_HARMONICS; k++) {
          const fh = f0 * k;
          if (fh > 4000) break;
          const formant = FORMANT_FLOOR +
            formantGain(fh, F[0], 50) * 1.0 +
            formantGain(fh, F[1], 70) * 0.85 +
            formantGain(fh, F[2], 90) * 0.6;
          const g = Math.min(formant, 1.9);
          s += (g / Math.pow(k, ROLLOFF_EXP)) * Math.sin(2 * Math.PI * fh * t);
        }
        s *= SIGNAL_GAIN * env;
      }
      s += noise() * NOISE_AMP * (env > 0.05 ? 1.3 : 1.0);
      if (s >  1) s =  1;
      if (s < -1) s = -1;
      oscBuf[oscPos] = s;
      oscPos = (oscPos + 1) % OSC_BUFFER;
      sampleClock++;
    }
  }

  // ── Synthetic standing-wave (THINKING + POLISHING) ──────────────────────
  const POLISH_PANEL_W = 588.0;
  const POLISH_F1_EFF  = 0.015 * POLISH_PANEL_W / (OSC_BUFFER - 1);
  const POLISH_F2_EFF  = 0.010 * POLISH_PANEL_W / (OSC_BUFFER - 1);
  const POLISH_AMP_DIV = 44.0;
  function fillStandingWave(timeMs) {
    const beatEnv = Math.sin(timeMs * 0.001);
    const tBg = timeMs * 0.002;
    const tFg = timeMs * 0.003;
    const inv = 1 / (OSC_BUFFER - 1);
    for (let i = 0; i < OSC_BUFFER; i++) {
      const bg_b1 = Math.sin(i * POLISH_F1_EFF + tBg)       * 20 * 0.6;
      const bg_b2 = Math.sin(i * POLISH_F2_EFF - tBg * 1.5) * 15 * 0.6 * beatEnv;
      const fg_b1 = Math.sin(i * POLISH_F1_EFF + tFg)       * 20 * 1.0;
      const fg_b2 = Math.sin(i * POLISH_F2_EFF - tFg * 1.5) * 15 * 1.0 * beatEnv;
      const yVal = fg_b1 + fg_b2 + 0.4 * (bg_b1 + bg_b2);
      const edge = Math.sin(i * inv * Math.PI);
      let v = (yVal * edge) / POLISH_AMP_DIV;
      if (v >  1) v =  1; else if (v < -1) v = -1;
      oscBuf[(oscPos + i) % OSC_BUFFER] = v;
    }
  }

  // ── Script content ──────────────────────────────────────────────────────
  const TURNS = [
    { kind: 'dictate',  text: "Push to talk. Speak naturally." },
    { kind: 'dictate',  text: "Fono types it for you." },
    { kind: 'dictate',  text: "Fully offline. Fully yours." },
    { kind: 'ask',
      q: "How do you say 'thank you' in Japanese?",
      a: "Arigatou gozaimasu." },
    { kind: 'ask',
      q: "What does FFT stand for?",
      a: "Fast Fourier Transform." },
    { kind: 'ask',
      q: "How do I exit Vim?",
      a: "Press Escape, then type :q and Enter." },
  ];

  // ── State machine ───────────────────────────────────────────────────────
  let turnIdx     = 0;
  let qChar       = 0;
  let aChar       = 0;
  let dChar       = 0;
  let scriptPhase = 'd-typing';
  let scriptPhaseAt = 0;
  let charPaceMs    = 0;

  function currentState() {
    switch (scriptPhase) {
      case 'd-typing':
      case 'd-hold':    return 'RECORDING';
      case 'd-polish':
      case 'd-clear':   return 'POLISHING';
      case 'a-typing':
      case 'a-hold':    return 'ASSISTANT';
      case 'a-think':
      case 'a-dwell':
      case 'a-clear':   return 'THINKING';
    }
    return 'RECORDING';
  }
  function accentFor(state) {
    switch (state) {
      case 'RECORDING': return ACCENT_RECORDING;
      case 'POLISHING': return ACCENT_POLISHING;
      case 'ASSISTANT': return ACCENT_ASSISTANT;
      case 'THINKING':  return ACCENT_THINKING;
    }
    return ACCENT_RECORDING;
  }
  function isStandingWavePhase(p) {
    return p === 'd-polish' || p === 'd-clear'
        || p === 'a-think'  || p === 'a-dwell' || p === 'a-clear';
  }

  // ── Drawing helpers ─────────────────────────────────────────────────────
  function rgba(rgb, a) {
    return 'rgba(' + rgb[0] + ',' + rgb[1] + ',' + rgb[2] + ',' + a + ')';
  }
  function roundRectPath(x, y, w, h, r) {
    r = Math.min(r, w / 2, h / 2);
    ctx.beginPath();
    ctx.moveTo(x + r, y);
    ctx.lineTo(x + w - r, y);
    ctx.quadraticCurveTo(x + w, y, x + w, y + r);
    ctx.lineTo(x + w, y + h - r);
    ctx.quadraticCurveTo(x + w, y + h, x + w - r, y + h);
    ctx.lineTo(x + r, y + h);
    ctx.quadraticCurveTo(x, y + h, x, y + h - r);
    ctx.lineTo(x, y + r);
    ctx.quadraticCurveTo(x, y, x + r, y);
    ctx.closePath();
  }

  function drawPanel(px, py, pw, ph, kind, state) {
    roundRectPath(px, py, pw, ph, CORNER_RADIUS);
    ctx.fillStyle = COLOR_BG;
    ctx.fill();
    const accent = accentFor(state);
    const stripeInset = CORNER_RADIUS * 0.4;
    roundRectPath(px, py + stripeInset, ACCENT_WIDTH, ph - stripeInset * 2,
                  ACCENT_WIDTH * 0.5);
    ctx.fillStyle = rgba(accent, 1);
    ctx.fill();
    ctx.fillStyle = COLOR_TEXT_DIM;
    ctx.font = '600 ' + STATUS_FONT_PX + 'px -apple-system, "Segoe UI", "DejaVu Sans", sans-serif';
    ctx.textBaseline = 'alphabetic';
    const pad_x = px + PADDING_X + ACCENT_WIDTH;
    ctx.fillText(kind === 'osc' ? state : 'TRANSCRIPT',
                 pad_x, py + PADDING_TOP + STATUS_FONT_PX * 0.85);

    const cx0 = pad_x;
    const cx1 = px + pw - PADDING_X;
    const cy0 = py + PADDING_TOP + STATUS_FONT_PX + STATUS_TO_TEXT;
    const cy1 = py + ph - PADDING_BOT;

    if (kind === 'osc') {
      const headroom = isStandingWavePhase(scriptPhase) ? OSC_HEADROOM_THINK
                                                         : OSC_HEADROOM_REC;
      drawOscilloscope(cx0, cy0, cx1, cy1, accent, headroom);
    } else {
      drawText(cx0, cy0, cx1, cy1);
    }
  }

  function drawOscilloscope(x0, y0, x1, y1, accent, headroom) {
    const areaW = Math.max(x1 - x0, 0);
    const areaH = Math.max(y1 - y0, 0);
    if (areaW < 1 || areaH < 1) return;
    const yMid = (y0 + y1) * 0.5;
    const halfH = areaH * 0.5 * headroom;

    ctx.fillStyle = 'rgba(170, 170, 178, 0.13)';
    ctx.fillRect(x0, Math.round(yMid), Math.round(areaW), 1);

    const cols = Math.max(1, Math.floor(areaW));
    const denom = Math.max(1, cols - 1);
    const xs = new Float32Array(cols);
    const ys = new Float32Array(cols);
    for (let px = 0; px < cols; px++) {
      const frac = px / denom;
      const sampleIdx = Math.floor(frac * (OSC_BUFFER - 1));
      const idx = (oscPos + sampleIdx) % OSC_BUFFER;
      const amp = Math.max(-1, Math.min(1, oscBuf[idx]));
      xs[px] = x0 + frac * (x1 - x0);
      ys[px] = Math.max(y0, Math.min(y1, yMid - amp * halfH));
    }

    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';

    ctx.lineWidth = 1.4;
    ctx.strokeStyle = rgba(accent, 0x80 / 0xFF);
    ctx.beginPath();
    ctx.moveTo(xs[0], ys[0] + 1);
    for (let i = 1; i < cols; i++) ctx.lineTo(xs[i], ys[i] + 1);
    ctx.stroke();

    ctx.lineWidth = 1.4;
    ctx.strokeStyle = rgba(accent, 1);
    ctx.beginPath();
    ctx.moveTo(xs[0], ys[0]);
    for (let i = 1; i < cols; i++) ctx.lineTo(xs[i], ys[i]);
    ctx.stroke();
  }

  function drawText(x0, y0, x1, y1) {
    ctx.font = '500 ' + TEXT_FONT_PX + 'px -apple-system, "Segoe UI", "DejaVu Sans", sans-serif';
    ctx.textBaseline = 'alphabetic';
    const cur = TURNS[turnIdx];

    if (cur.kind === 'dictate') {
      if (scriptPhase === 'd-clear') return;
      const line = cur.text.slice(0, dChar);
      const y = y0 + TEXT_FONT_PX * 0.85;
      ctx.fillStyle = (scriptPhase === 'd-typing') ? COLOR_TEXT : COLOR_TEXT_DIM;
      ctx.fillText(line, x0, y);
      if (scriptPhase === 'd-typing') drawCaret(line, x0, y);
      return;
    }

    if (scriptPhase === 'a-clear') return;
    const qText = cur.q.slice(0, qChar);
    const inThink = scriptPhase === 'a-think' || scriptPhase === 'a-dwell';
    const aText = inThink ? cur.a.slice(0, aChar) : '';

    const qLine = '\u203A  ' + qText;
    let y = y0 + TEXT_FONT_PX * 0.85;
    ctx.fillStyle = (scriptPhase === 'a-typing') ? COLOR_TEXT : COLOR_TEXT_DIM;
    ctx.fillText(qLine, x0, y);
    if (scriptPhase === 'a-typing') drawCaret(qLine, x0, y);

    if (aText.length > 0) {
      const aLine = '\u2039  ' + aText;
      y += TEXT_FONT_PX + LINE_GAP;
      ctx.fillStyle = (scriptPhase === 'a-think') ? COLOR_TEXT : COLOR_TEXT_DIM;
      ctx.fillText(aLine, x0, y);
      if (scriptPhase === 'a-think') drawCaret(aLine, x0, y);
    }
  }

  function drawCaret(line, x0, baselineY) {
    const blink = (Math.floor(virtualMs / 500) % 2) === 0;
    if (!blink) return;
    const w = ctx.measureText(line).width;
    ctx.fillRect(x0 + w + 2, baselineY - TEXT_FONT_PX * 0.85, 2, TEXT_FONT_PX);
  }

  // ── Main loop ───────────────────────────────────────────────────────────
  let virtualMs = 0;
  let lastSampleMs = 0;
  let lastRaf = 0;
  let paused = false;

  const D_HOLD_MS    = 500;
  const D_POLISH_MS  = 1000;
  const D_CLEAR_MS   = 250;
  const A_HOLD_MS    = 400;
  const A_DWELL_MS   = 1400;
  const A_CLEAR_MS   = 250;
  const CHAR_MS_VOICE = 35;
  const CHAR_MS_REPLY = 22;
  const ACTIVE_THRESH = 0.18;

  function advanceTurn() {
    turnIdx = (turnIdx + 1) % TURNS.length;
    const next = TURNS[turnIdx];
    qChar = 0; aChar = 0; dChar = 0;
    charPaceMs = 0;
    scriptPhase = (next.kind === 'dictate') ? 'd-typing' : 'a-typing';
    scriptPhaseAt = virtualMs;
    sampleClock = 0;
    noiseState = 0;
    emitTurn(next.kind);
  }

  // Broadcast turn-kind changes so the page can react (e.g. swap the
  // hero headline between "It types." / "It answers." and recolor the
  // matching word in the lede). Fires once per kind transition.
  let lastEmittedKind = null;
  function emitTurn(kind) {
    if (kind === lastEmittedKind) return;
    lastEmittedKind = kind;
    wrap.dispatchEvent(new CustomEvent('fono-demo-turn', {
      detail: { kind },
      bubbles: true,
    }));
  }

  function tick(now) {
    if (paused) { lastRaf = now; return; }
    if (!lastRaf) lastRaf = now;
    const dt = Math.min(now - lastRaf, 100);
    lastRaf = now;
    virtualMs += dt;

    if (isStandingWavePhase(scriptPhase)) {
      fillStandingWave(virtualMs);
      const wantSample = Math.floor(virtualMs * SAMPLE_RATE * PRODUCER_SPEED / 1000);
      sampleClock = wantSample;
      lastSampleMs = virtualMs;
    } else {
      const wantSample = Math.floor(virtualMs * SAMPLE_RATE * PRODUCER_SPEED / 1000);
      const lastSample = Math.floor(lastSampleMs * SAMPLE_RATE * PRODUCER_SPEED / 1000);
      const need = wantSample - lastSample;
      if (need > 0) generateSamples(need);
      lastSampleMs = virtualMs;
    }

    const env = envelope(virtualMs / 1000);
    const cur = TURNS[turnIdx];

    switch (scriptPhase) {
      case 'd-typing':
        if (env > ACTIVE_THRESH) {
          charPaceMs += dt;
          while (charPaceMs >= CHAR_MS_VOICE && dChar < cur.text.length) {
            dChar++;
            charPaceMs -= CHAR_MS_VOICE;
          }
        } else {
          charPaceMs = 0;
        }
        if (dChar >= cur.text.length) {
          scriptPhase = 'd-hold'; scriptPhaseAt = virtualMs;
        }
        break;
      case 'd-hold':
        if (virtualMs - scriptPhaseAt > D_HOLD_MS) {
          scriptPhase = 'd-polish'; scriptPhaseAt = virtualMs;
        }
        break;
      case 'd-polish':
        if (virtualMs - scriptPhaseAt > D_POLISH_MS) {
          scriptPhase = 'd-clear'; scriptPhaseAt = virtualMs;
        }
        break;
      case 'd-clear':
        if (virtualMs - scriptPhaseAt > D_CLEAR_MS) advanceTurn();
        break;

      case 'a-typing':
        if (env > ACTIVE_THRESH) {
          charPaceMs += dt;
          while (charPaceMs >= CHAR_MS_VOICE && qChar < cur.q.length) {
            qChar++;
            charPaceMs -= CHAR_MS_VOICE;
          }
        } else {
          charPaceMs = 0;
        }
        if (qChar >= cur.q.length) {
          scriptPhase = 'a-hold'; scriptPhaseAt = virtualMs;
        }
        break;
      case 'a-hold':
        if (virtualMs - scriptPhaseAt > A_HOLD_MS) {
          scriptPhase = 'a-think'; scriptPhaseAt = virtualMs;
          aChar = 0; charPaceMs = 0;
        }
        break;
      case 'a-think':
        charPaceMs += dt;
        while (charPaceMs >= CHAR_MS_REPLY && aChar < cur.a.length) {
          aChar++;
          charPaceMs -= CHAR_MS_REPLY;
        }
        if (aChar >= cur.a.length) {
          scriptPhase = 'a-dwell'; scriptPhaseAt = virtualMs;
        }
        break;
      case 'a-dwell':
        if (virtualMs - scriptPhaseAt > A_DWELL_MS) {
          scriptPhase = 'a-clear'; scriptPhaseAt = virtualMs;
        }
        break;
      case 'a-clear':
        if (virtualMs - scriptPhaseAt > A_CLEAR_MS) advanceTurn();
        break;
    }

    render();
    rafHandle = requestAnimationFrame(tick);
  }

  function render() {
    ctx.clearRect(0, 0, W_CSS, H_CSS);
    const state = currentState();
    drawPanel(10,  10, W_CSS - 20, 130, 'osc',  state);
    drawPanel(10, 160, W_CSS - 20, 130, 'text', state);
  }

  // ── prefers-reduced-motion / pause-on-hidden / play pill ────────────────
  let rafHandle = 0;
  function start() {
    paused = false;
    wrap.dataset.paused = 'false';
    lastRaf = 0;
    rafHandle = requestAnimationFrame(tick);
  }
  function stop() {
    paused = true;
    wrap.dataset.paused = 'true';
    if (rafHandle) cancelAnimationFrame(rafHandle);
    rafHandle = 0;
  }

  document.addEventListener('visibilitychange', () => {
    if (document.hidden) stop();
    else if (!reducedMotion.matches) start();
  });
  pill.addEventListener('click', () => { if (paused) start(); });

  const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
  function renderStill() {
    virtualMs = 700;
    generateSamples(Math.floor(SAMPLE_RATE * 0.7));
    turnIdx = 0;
    dChar = Math.floor(TURNS[0].text.length * 0.7);
    scriptPhase = 'd-typing';
    render();
  }
  if (reducedMotion.matches) {
    renderStill();
    paused = true;
    wrap.dataset.paused = 'true';
  } else {
    start();
  }
  reducedMotion.addEventListener?.('change', e => {
    if (e.matches) { stop(); renderStill(); }
    else           { start(); }
  });
})();
