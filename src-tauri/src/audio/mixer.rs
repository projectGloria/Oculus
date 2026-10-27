use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::wasapi::Packet;
use crate::clock::Clock;
use crate::encode::mf_writer::{AUDIO_CHANNELS, AUDIO_RATE};

const CH: usize = AUDIO_CHANNELS as usize;
const RATE: i64 = AUDIO_RATE as i64;
/// Jitter/drift we absorb by just appending. Beyond this a source is re-aligned to its QPC
/// timestamp: zero-fill if it's behind (loopback went silent, device clock slow), drop the
/// overlap if it's ahead (device clock fast). Bounded drift, no resampler.
const TOLERANCE: i64 = RATE / 50; // 20ms
/// How far behind "now" the mixer emits, so late packets still land in place.
const LATENCY: i64 = RATE / 10; // 100ms
const MIN_CHUNK: i64 = RATE / 100; // 10ms

fn hns_to_frames(hns: i64) -> i64 {
    (hns as i128 * RATE as i128 / 10_000_000) as i64
}

/// One source placed on the recording timeline. `buf[0]` is timeline frame `base`.
#[derive(Default)]
pub struct Track {
    buf: VecDeque<f32>,
    base: i64,
}

impl Track {
    fn end(&self) -> i64 {
        self.base + (self.buf.len() / CH) as i64
    }

    /// Place a packet whose first frame belongs at timeline frame `at`. `emitted` is how far
    /// the mixer has already output; nothing may land before it.
    pub fn push(&mut self, at: i64, samples: &[f32], emitted: i64) {
        if self.buf.is_empty() {
            self.base = self.base.max(emitted);
        }
        let gap = at - self.end();
        let mut s = samples;
        if gap > TOLERANCE {
            // Source was silent (WASAPI loopback emits *nothing* when nothing plays) or
            // drifted slow: fill the hole so later audio stays aligned with video.
            self.buf.extend(std::iter::repeat_n(0.0, gap as usize * CH));
        } else if gap < -TOLERANCE {
            let skip = ((-gap) as usize * CH).min(s.len());
            s = &s[skip..];
        }
        self.buf.extend(s.iter().copied());
    }

    /// Mix `n` frames starting at timeline frame `from` into `out`, zero where missing.
    pub fn pull_into(&mut self, from: i64, n: usize, out: &mut [f32], gain: f32) {
        if self.base < from {
            // Stale data (shouldn't happen given `push` clamps to `emitted`): discard.
            let stale = (((from - self.base) as usize) * CH).min(self.buf.len());
            self.buf.drain(..stale);
            self.base += (stale / CH) as i64;
            if self.buf.is_empty() {
                self.base = from;
            }
        }
        let lead = ((self.base - from).max(0) as usize).min(n);
        let take = (n - lead).min(self.buf.len() / CH);
        for (i, v) in self.buf.drain(..take * CH).enumerate() {
            out[lead * CH + i] += v * gain;
        }
        if lead == 0 {
            self.base += take as i64;
        }
        if self.buf.is_empty() {
            self.base = self.base.max(from + n as i64);
        }
    }
}

pub struct Gains {
    pub system: f32,
    pub mic: f32,
}

/// Mixes system + mic onto the shared clock and hands PCM16 chunks to `sink(pcm, pts)`.
/// Ticks on its own timer (not on packet arrival), so a silent loopback never stalls the mic.
pub fn spawn<F>(rx: Receiver<Packet>, clock: Arc<Clock>, gains: Gains, stop: Arc<AtomicBool>, mut sink: F) -> JoinHandle<()>
where
    F: FnMut(Vec<i16>, i64) + Send + 'static,
{
    thread::Builder::new()
        .name("oculus-mixer".into())
        .spawn(move || {
            let mut tracks = [Track::default(), Track::default()];
            let gain = [gains.system, gains.mic];
            let mut emitted: i64 = 0;
            let place = |p: Packet, tracks: &mut [Track; 2], emitted: i64| {
                let (mut qpc, mut s) = (p.qpc, &p.samples[..]);
                // Packet straddling the start latch: keep only the part after frame 0.
                if let Some(start) = clock.start() {
                    if qpc < start {
                        let skip = (hns_to_frames(start - qpc) as usize * CH).min(s.len());
                        s = &s[skip..];
                        qpc = start;
                    }
                }
                if let Some(pts) = clock.pts(qpc) {
                    tracks[p.source as usize].push(hns_to_frames(pts), s, emitted);
                }
            };
            let mut emit = |upto: i64, tracks: &mut [Track; 2], emitted: &mut i64| {
                while upto - *emitted >= MIN_CHUNK {
                    let n = (upto - *emitted).min(RATE) as usize;
                    let mut mix = vec![0.0f32; n * CH];
                    for (t, g) in tracks.iter_mut().zip(gain) {
                        t.pull_into(*emitted, n, &mut mix, g);
                    }
                    let pcm = mix.iter().map(|v| (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).collect();
                    sink(pcm, *emitted * 10_000_000 / RATE);
                    *emitted += n as i64;
                }
            };

            loop {
                match rx.recv_timeout(Duration::from_millis(10)) {
                    Ok(p) => place(p, &mut tracks, emitted),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                while let Ok(p) = rx.try_recv() {
                    place(p, &mut tracks, emitted);
                }
                let Some(now) = clock.now() else { continue };
                if stop.load(Ordering::Relaxed) {
                    // Final flush right up to "now" so audio doesn't end 100ms before video.
                    emit(hns_to_frames(now), &mut tracks, &mut emitted);
                    break;
                }
                emit(hns_to_frames(now) - LATENCY, &mut tracks, &mut emitted);
            }
        })
        .expect("spawn mixer")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(n: usize, v: f32) -> Vec<f32> {
        vec![v; n * CH]
    }

    #[test]
    fn silence_gap_is_zero_filled() {
        let mut t = Track::default();
        t.push(0, &frames(480, 1.0), 0);
        // Loopback went quiet for 1s, then resumes: must land at frame 48_480, not 480.
        t.push(48_480, &frames(480, 1.0), 0);
        let mut out = vec![0.0; 48_960 * CH];
        t.pull_into(0, 48_960, &mut out, 1.0);
        assert_eq!(out[0], 1.0);
        assert_eq!(out[1000 * CH], 0.0);
        assert_eq!(out[48_500 * CH], 1.0);
    }

    #[test]
    fn small_jitter_is_absorbed() {
        let mut t = Track::default();
        t.push(0, &frames(480, 1.0), 0);
        t.push(490, &frames(480, 1.0), 0); // 10 frames late: within tolerance, appended
        assert_eq!(t.end(), 960);
    }

    #[test]
    fn overlap_beyond_tolerance_is_dropped() {
        let mut t = Track::default();
        t.push(0, &frames(4800, 1.0), 0);
        t.push(2000, &frames(4800, 1.0), 0); // device clock ran fast: 2800 frames overlap
        assert_eq!(t.end(), 6800);
    }

    #[test]
    fn missing_source_pulls_zeros() {
        let mut t = Track::default();
        let mut out = vec![0.5; 480 * CH];
        t.pull_into(0, 480, &mut out, 1.0);
        assert!(out.iter().all(|&v| v == 0.5));
        assert_eq!(t.base, 480);
    }

    #[test]
    fn late_packet_never_lands_before_emitted() {
        let mut t = Track::default();
        t.push(0, &frames(480, 1.0), 10_000);
        assert_eq!(t.base, 10_000);
    }
}
