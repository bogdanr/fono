# Speaker verification (who is speaking)

Fono can recognise **who is speaking** and tag each dictation with the
matching enrolled person — entirely on-device. It is off by default.

This is useful when more than one person uses the same machine (or the
same LAN mic), when you want history attributed per speaker, or as a
lightweight gate so only enrolled voices trigger certain flows.

> **Identification, not authentication.** A voiceprint is a convenience
> signal, not a password: it can be defeated by a recording or a good
> impersonation. **Never** gate an irreversible or fail-deadly action on
> the voice channel alone — always require a second factor outside the
> microphone.

## Privacy at a glance

- The model turns your audio into a numeric **voiceprint (embedding)**.
  The raw audio is dropped after the embedding is computed; it is never
  written to disk.
- Voiceprints live in `speakers.sqlite` (mode `0600`) under
  `$XDG_DATA_HOME/fono/` and **never leave the machine**.
- The embedding is **never** attached to a cloud STT or LLM request —
  turning verification on does not change what is sent to a provider (a
  regression test enforces this). At most a matched speaker's **name** is
  stored in the local history database.

See [privacy.md](privacy.md) for the full data-flow statement.

## Turning it on

Enable it in `config.toml` (or the browser settings **Speakers** page):

```toml
[speaker]
enabled = true              # off by default; no model is loaded until true
model = "redimnet2-b3"      # "-b6" is the larger, slightly more accurate tier
threshold = "auto"          # see "Thresholds" below
min_speech_secs = 3.0       # speech gathered before a decision is made
```

The first time it is enabled, Fono downloads the small embedding model
(and its impostor cohort) from the voice mirror into the model cache. All
config keys are documented in
[configuration.md → Speaker verification](configuration.md#speaker-verification-who-is-speaking).

## Enrolling your voice

Enrollment records a few short clips and stores only their voiceprints.

### From the browser settings page (recommended)

1. Open settings — tray **Settings…** or `fono config web` — and go to the
   **Speakers** section.
2. In the **enrollment card**, type a name, pick your microphone, and
   record. A live level meter shows your input; Fono warns if a clip is
   too quiet, clipping, or recorded in a noisy room.
3. Record several clips (ideally on the mic and in the room you actually
   dictate in). After each clip Fono shows whether the sample matches the
   profile so far, and a **profile-strength** badge (weak / ok / strong)
   nudges you toward enough good audio.

Capture uses the browser mic with its DSP (echo-cancel / noise-suppression
/ auto-gain) disabled and resamples to 16 kHz mono, so the enrolled
voiceprint matches what the daemon hears at dictation time.

### Manage existing speakers

The roster lists each enrolled person with their sample count, whether
they've been calibrated, and when they were last updated. From there you
can rename or remove a speaker, or open **Manage samples** to review
individual clips (with their capture quality) and prune weak ones — Fono
suggests which samples to drop while preserving a safe minimum.

## "Test my voice" (calibration)

Enrollment tells Fono what your voice *is*; calibration tells it how
confidently it can tell you apart from everyone else — and picks a good
threshold for you.

1. On the **Speakers** page, use the **test my voice** card to record a
   few short *held-out* clips (recordings you did **not** use for
   enrollment) — at least two, though three to five is better.
2. Fono scores those clips against your enrolled profile and against a
   shipped impostor cohort, then shows:
   - a "you vs others" score histogram, with each group scaled to its own
     height so your handful of clips stays visible next to the large
     impostor set;
   - your **self error-rate (EER)** and a plain-language verdict;
   - the measured per-clip embedding latency on this machine;
   - three cut-offs marked on the chart — **Auto** (the adaptive point
     `threshold = "auto"` enforces at dictation time), **Fixed** (a
     rounded value set halfway between Auto and the measured balance
     point), and the strict **Safety** floor the Fixed value never drops
     below.
3. Leave `[speaker].threshold` on `"auto"` for an operating point that
   adapts as your mic and room change, or click **Pin a fixed threshold**
   to write the rounded Fixed value into `[speaker].threshold`.

The saved calibration is what `threshold = "auto"` uses, so testing your
voice directly improves accuracy.

## Thresholds

`[speaker].threshold` decides how close a match must be to accept:

- **`"auto"` (default)** — resolved from the shipped impostor cohort plus
  your own calibration stats. Good for most people; it adapts as you run
  "test my voice".
- **A fixed float** — pins a strict operating point that never moves.
  Prefer this for deployments where you want a predictable false-accept
  rate regardless of calibration.

`min_speech_secs` controls how much speech (the wake phrase plus the
command) is gathered before a decision. Short commands keep accumulating
audio until the minimum is met, so a one-word command doesn't force a
low-confidence guess.

## Command line

Everything the web page does has a terminal equivalent:

```console
$ fono speaker list                 # id, name, sample count, calibrated?, updated
$ fono speaker rename 2 alice       # rename speaker id 2
$ fono speaker remove 2             # delete a speaker and all their voiceprints
$ fono speaker test 2 a.wav b.wav   # "test my voice" from held-out WAV clips
```

`fono speaker test` loads one or more held-out 16-bit PCM WAV clips
(non-16 kHz files are resampled), scores them exactly like the web card,
prints the score distributions, self-EER, recommended and strict
thresholds, and per-embed latency, and **saves the calibration** so
`threshold = "auto"` can use it.

`fono doctor` includes a **Speaker** section: whether verification is
enabled, whether the configured `[speaker].model` is a known registry
model, the threshold source, and the enrolled-speaker count (with a
warning if verification is on but nobody is enrolled).

## How it works (brief)

1. The embedding model (ReDimNet2, `redimnet2-b3` by default, or the
   larger `-b6`) turns 16 kHz mono audio into a fixed-length voiceprint.
2. At dictation time Fono embeds the captured speech and compares it to
   each enrolled speaker's voiceprint using **AS-Norm** score
   normalisation against a shipped impostor cohort, which makes scores
   comparable across voices and rooms.
3. If the best score clears the threshold, the transcript is tagged with
   that speaker's name; otherwise it stays untagged. The embedding runs
   concurrently with speech-to-text, so it adds no perceptible latency.

## Limitations

- **Not a security boundary** — see the note at the top.
- Accuracy depends on enrollment quality. Enroll on the mic and in the
  room you actually use; run "test my voice" to confirm.
- Very short utterances below `min_speech_secs` accumulate more audio
  before a decision, so the very first words may be untagged.
