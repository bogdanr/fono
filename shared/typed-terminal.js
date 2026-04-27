// Lightweight typewriter that simulates dictation transcription.
// Words "appear" with occasional re-corrections (LLM cleanup vibe).
(function () {
  function typeSequence(el, sequence, opts) {
    opts = opts || {};
    const charDelay = opts.charDelay || 28;
    const wordPause = opts.wordPause || 90;
    const linePause = opts.linePause || 600;
    const loop = opts.loop !== false;
    const caret = opts.caret !== false;
    let cancelled = false;

    el.innerHTML = '';
    const out = document.createElement('span');
    out.className = 'tt-out';
    el.appendChild(out);
    if (caret) {
      const c = document.createElement('span');
      c.className = 'tt-caret';
      c.textContent = '▍';
      el.appendChild(c);
    }

    async function sleep(ms) { return new Promise(r => setTimeout(r, ms)); }

    async function run() {
      while (!cancelled) {
        for (const item of sequence) {
          if (cancelled) return;
          if (item.type === 'clear') { out.innerHTML = ''; await sleep(item.delay || 200); continue; }
          if (item.type === 'pause') { await sleep(item.delay || 400); continue; }
          if (item.type === 'line') {
            const words = item.text.split(' ');
            for (let w = 0; w < words.length; w++) {
              const word = (w === 0 ? '' : ' ') + words[w];
              for (const ch of word) {
                if (cancelled) return;
                out.appendChild(document.createTextNode(ch));
                await sleep(charDelay + (Math.random() * 20 - 10));
              }
              await sleep(wordPause);
            }
            out.appendChild(document.createTextNode('\n'));
            await sleep(linePause);
          }
          if (item.type === 'correct') {
            const node = out;
            const txt = node.textContent;
            const target = item.find;
            const idx = txt.lastIndexOf(target);
            if (idx >= 0) {
              for (let i = 0; i < target.length; i++) {
                if (cancelled) return;
                node.textContent = node.textContent.slice(0, -1);
                await sleep(15);
              }
              for (const ch of item.replace) {
                if (cancelled) return;
                node.appendChild(document.createTextNode(ch));
                await sleep(charDelay);
              }
              await sleep(300);
            }
          }
        }
        if (!loop) break;
        await sleep(1400);
        out.innerHTML = '';
      }
    }
    run();
    return () => { cancelled = true; };
  }
  window.typeSequence = typeSequence;
})();
