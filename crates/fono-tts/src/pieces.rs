// SPDX-License-Identifier: GPL-3.0-only
//! Speak a finished text in pieces, so the first audio comes out after the
//! first sentence rather than after the whole reply.
//!
//! The text is cut into sentences, and a sentence of fewer than
//! [`MIN_WORDS`] words joins the next one, so "Sure." never goes out alone.
//! The first sentence is its own piece: that is what gets sound out early.
//! After that, sentences are grouped into pieces sized from how fast the
//! provider actually answered: a piece holds as much speech as the provider
//! can make in three quarters of the time the audio already queued takes to
//! play. A fast
//! cloud provider gets the rest of a short reply in one or two requests; a
//! slow one gets smaller pieces, so playback never runs dry.

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::Result;
use futures::stream::{BoxStream, StreamExt};

use crate::{SentenceSplitter, TextToSpeech, TtsChunk};

/// Sentences shorter than this many words join the next sentence.
pub const MIN_WORDS: usize = 4;

/// Share of the queued audio's playing time a piece may take to make. The
/// rest is a margin for a request that is slower than the ones before it.
/// The measured speed includes each request's fixed overhead, which a
/// bigger piece pays only once, so the estimate already errs on the slow
/// side.
const SAFETY: f32 = 0.75;

/// The plan for one text: what to synthesise next, given how much audio
/// is already out. Text can arrive all at once ([`Pieces::new`]) or bit by
/// bit as a model writes it ([`Pieces::push`], then [`Pieces::finish`]).
#[derive(Debug, Default)]
pub struct Pieces {
    splitter: SentenceSplitter,
    /// A sentence too short to go alone, waiting for the next one.
    carry: String,
    sentences: VecDeque<String>,
    finished: bool,
    /// Characters, audio seconds and wall seconds of every finished piece.
    chars_done: usize,
    produced_secs: f32,
    took_secs: f32,
    /// How long the first piece took: playback starts when it arrives.
    first_took: Option<f32>,
}

impl Pieces {
    /// The plan for a text that is already complete.
    #[must_use]
    pub fn new(text: &str) -> Self {
        let mut p = Self::default();
        p.push(text);
        p.finish();
        p
    }

    /// Add text as it arrives.
    pub fn push(&mut self, text: &str) {
        for s in self.splitter.push(text) {
            self.add_sentence(&s);
        }
    }

    /// No more text is coming.
    pub fn finish(&mut self) {
        if let Some(s) = self.splitter.flush() {
            self.add_sentence(&s);
        }
        if !self.carry.is_empty() {
            let carry = std::mem::take(&mut self.carry);
            match self.sentences.back_mut() {
                Some(last) => {
                    last.push(' ');
                    last.push_str(&carry);
                }
                None => self.sentences.push_back(carry),
            }
        }
        self.finished = true;
    }

    /// `true` once [`Self::finish`] was called and every piece was taken.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.finished && self.sentences.is_empty()
    }

    fn add_sentence(&mut self, s: &str) {
        if !self.carry.is_empty() {
            self.carry.push(' ');
        }
        self.carry.push_str(s);
        if self.carry.split_whitespace().count() >= MIN_WORDS {
            self.sentences.push_back(std::mem::take(&mut self.carry));
        }
    }

    /// Record a finished piece: its length in characters, the seconds of
    /// audio it made, and the seconds it took to make.
    pub fn piece_done(&mut self, chars: usize, audio_secs: f32, took_secs: f32) {
        self.chars_done += chars;
        self.produced_secs += audio_secs;
        self.took_secs += took_secs;
        self.first_took.get_or_insert(took_secs);
    }

    /// How many characters the next piece may hold: as many as the provider,
    /// at the speed it has shown so far, can turn into audio in
    /// [`SAFETY`] of the time the queued audio takes to play.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn budget_chars(&self) -> usize {
        let Some(first_took) = self.first_took else { return 0 };
        if self.produced_secs <= 0.0 || self.took_secs <= 0.0 {
            return 0;
        }
        // Audio seconds made per second of waiting, request overhead included.
        let speed = self.produced_secs / self.took_secs;
        // Playback began when the first piece arrived; pieces are made back
        // to back, so it has been playing for the time spent since.
        let queued = (self.produced_secs - (self.took_secs - first_took)).max(0.0);
        let chars_per_sec = self.chars_done as f32 / self.produced_secs;
        (SAFETY * queued * speed * chars_per_sec) as usize
    }

    /// The next piece to synthesise, or `None` when no complete sentence is
    /// waiting.
    pub fn next_piece(&mut self) -> Option<String> {
        let mut piece = self.sentences.pop_front()?;
        let budget = self.budget_chars();
        while let Some(next) = self.sentences.front() {
            if piece.chars().count() + 1 + next.chars().count() > budget {
                break;
            }
            let next = self.sentences.pop_front().expect("front exists");
            piece.push(' ');
            piece.push_str(&next);
        }
        Some(piece)
    }
}

/// Synthesise `text` piece by piece with `tts`, yielding audio as each piece
/// is made. The stream ends with an empty chunk that has `is_final` set. A piece goes through
/// [`TextToSpeech::synthesize_stream`], so a backend that streams on its own
/// sends audio even sooner.
pub fn synthesize_in_pieces(
    tts: Arc<dyn TextToSpeech>,
    text: &str,
    voice: Option<String>,
    lang: Option<String>,
) -> BoxStream<'static, Result<TtsChunk>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = tx.send(text.to_owned());
    drop(tx);
    synthesize_arriving_text(tts, rx, voice, lang)
}

/// Like [`synthesize_in_pieces`], for text that arrives bit by bit on
/// `text`. Closing the channel means the text is complete.
pub fn synthesize_arriving_text(
    tts: Arc<dyn TextToSpeech>,
    text: tokio::sync::mpsc::UnboundedReceiver<String>,
    voice: Option<String>,
    lang: Option<String>,
) -> BoxStream<'static, Result<TtsChunk>> {
    struct State {
        tts: Arc<dyn TextToSpeech>,
        text: tokio::sync::mpsc::UnboundedReceiver<String>,
        plan: Pieces,
        voice: Option<String>,
        lang: Option<String>,
        current: Option<BoxStream<'static, Result<TtsChunk>>>,
        /// The piece being made: characters, audio seconds so far, start.
        piece: (usize, f32, std::time::Instant),
        /// Sample rate of the audio so far, for the closing empty chunk.
        rate: u32,
        done: bool,
    }
    let rate = tts.native_sample_rate();
    let state = State {
        tts,
        text,
        plan: Pieces::default(),
        voice,
        lang,
        current: None,
        piece: (0, 0.0, std::time::Instant::now()),
        rate,
        done: false,
    };
    futures::stream::unfold(state, |mut st| async move {
        if st.done {
            return None;
        }
        loop {
            if st.current.is_none() {
                // Take in all text that has already arrived, and notice
                // that it is complete, before choosing a piece: otherwise
                // the last sentence, still in the splitter, would always
                // go out on its own.
                if !st.plan.finished {
                    loop {
                        match st.text.try_recv() {
                            Ok(t) => st.plan.push(&t),
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                                st.plan.finish();
                                break;
                            }
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                        }
                    }
                }
                let Some(piece) = st.plan.next_piece() else {
                    if !st.plan.is_done() {
                        match st.text.recv().await {
                            Some(t) => st.plan.push(&t),
                            None => st.plan.finish(),
                        }
                        continue;
                    }
                    // Audio goes out the moment it is made, so the end is
                    // marked by an empty final chunk rather than by holding
                    // the last one back.
                    st.done = true;
                    let end = TtsChunk { pcm: Vec::new(), sample_rate: st.rate, is_final: true };
                    return Some((Ok(end), st));
                };
                st.piece = (piece.chars().count(), 0.0, std::time::Instant::now());
                match st
                    .tts
                    .synthesize_stream(&piece, st.voice.as_deref(), st.lang.as_deref())
                    .await
                {
                    Ok(s) => st.current = Some(s),
                    Err(e) => {
                        st.done = true;
                        return Some((Err(e), st));
                    }
                }
            }
            let next = st.current.as_mut().expect("set above").next().await;
            match next {
                None => {
                    let (chars, audio, started) = st.piece;
                    st.plan.piece_done(chars, audio, started.elapsed().as_secs_f32());
                    st.current = None;
                }
                Some(Err(e)) => {
                    st.done = true;
                    return Some((Err(e), st));
                }
                Some(Ok(mut chunk)) => {
                    #[allow(clippy::cast_precision_loss)]
                    let secs = chunk.pcm.len() as f32 / chunk.sample_rate.max(1) as f32;
                    st.piece.1 += secs;
                    st.rate = chunk.sample_rate;
                    chunk.is_final = false;
                    return Some((Ok(chunk), st));
                }
            }
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TtsAudio;
    use async_trait::async_trait;

    #[test]
    fn short_sentences_join_the_next_one() {
        let mut p = Pieces::new("Sure. The kitchen light is on now. Okay.");
        assert_eq!(p.next_piece().as_deref(), Some("Sure. The kitchen light is on now. Okay."));
        assert_eq!(p.next_piece(), None);
    }

    const S: &str = "This is a sentence of about three seconds long.";

    #[test]
    fn the_first_sentence_goes_alone() {
        let mut p = Pieces::new(&format!("{S} {S} {S}"));
        assert_eq!(p.next_piece().as_deref(), Some(S));
    }

    /// A fast provider: 3 s of audio in 0.6 s. With 3 s queued it can make
    /// 0.75 × 3 × 5 ≈ 11 s of speech in time, so the next piece holds three
    /// of these sentences.
    #[test]
    fn a_fast_provider_gets_bigger_pieces() {
        let mut p = Pieces::new(&[S; 6].join(" "));
        p.next_piece();
        p.piece_done(S.len(), 3.0, 0.6);
        assert_eq!(p.next_piece().unwrap().matches("This is").count(), 3);
    }

    /// When the text is complete, the rest of it is one piece if the
    /// provider is fast enough: four sentences make two requests.
    #[tokio::test]
    async fn a_complete_text_from_a_fast_provider_needs_two_requests() {
        struct Fast(std::sync::Mutex<usize>);
        #[async_trait]
        impl TextToSpeech for Fast {
            async fn synthesize(
                &self,
                text: &str,
                _: Option<&str>,
                _: Option<&str>,
            ) -> Result<TtsAudio> {
                *self.0.lock().unwrap() += 1;
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                // About 15 characters per second of speech, at 100 Hz.
                Ok(TtsAudio { pcm: vec![0.0; text.len() * 100 / 15], sample_rate: 100 })
            }
            fn name(&self) -> &'static str {
                "fast"
            }
            fn native_sample_rate(&self) -> u32 {
                100
            }
        }
        let tts = Arc::new(Fast(std::sync::Mutex::new(0)));
        let text = [S; 4].join(" ");
        let n = synthesize_in_pieces(tts.clone(), &text, None, None).count().await;
        assert!(n > 0);
        assert_eq!(*tts.0.lock().unwrap(), 2);
    }

    /// A provider slower than playback gets one sentence at a time: a
    /// bigger piece would leave a silence.
    #[test]
    fn a_slow_provider_gets_one_sentence_at_a_time() {
        let mut p = Pieces::new(&[S; 4].join(" "));
        p.next_piece();
        p.piece_done(S.len(), 3.0, 6.0);
        assert_eq!(p.next_piece().as_deref(), Some(S));
    }

    /// One second of audio per word, so the test can count what came out.
    struct WordClock;

    #[async_trait]
    impl TextToSpeech for WordClock {
        async fn synthesize(
            &self,
            text: &str,
            _: Option<&str>,
            _: Option<&str>,
        ) -> Result<TtsAudio> {
            Ok(TtsAudio { pcm: vec![0.0; 10 * text.split_whitespace().count()], sample_rate: 10 })
        }
        fn name(&self) -> &'static str {
            "word-clock"
        }
        fn native_sample_rate(&self) -> u32 {
            10
        }
    }

    #[tokio::test]
    async fn text_that_arrives_in_bits_is_spoken_in_full() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        for bit in ["One two ", "three four five. Six", " seven eight nine ten."] {
            tx.send(bit.to_string()).unwrap();
        }
        drop(tx);
        let chunks: Vec<TtsChunk> = synthesize_arriving_text(Arc::new(WordClock), rx, None, None)
            .map(|c| c.unwrap())
            .collect()
            .await;
        assert_eq!(chunks.iter().map(|c| c.pcm.len()).sum::<usize>(), 10 * 10);
        assert!(chunks.last().unwrap().is_final);
    }

    #[tokio::test]
    async fn every_word_comes_out_and_an_empty_chunk_marks_the_end() {
        let text =
            "One two three four five. Six seven eight nine ten. Eleven twelve thirteen fourteen.";
        let chunks: Vec<TtsChunk> = synthesize_in_pieces(Arc::new(WordClock), text, None, None)
            .map(|c| c.unwrap())
            .collect()
            .await;
        assert_eq!(chunks.iter().map(|c| c.pcm.len()).sum::<usize>(), 14 * 10);
        assert!(chunks.len() >= 2, "the first sentence goes out on its own");
        let (end, audio) = chunks.split_last().unwrap();
        assert!(end.is_final && end.pcm.is_empty());
        assert!(audio.iter().all(|c| !c.is_final && !c.pcm.is_empty()));
    }
}
