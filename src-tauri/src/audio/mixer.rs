use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::wasapi::Packet;
use crate::clock::Clock;
use crate::encode::mf_writer::{AUDIO_CHANNELS, AUDIO_RATE};
use crate::queue::Receiver;

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
    pub fn push(&mut self, at: i64, samples: &[f32], emitted: i64) -> (u64, u64) {
        if self.buf.is_empty() {
            self.base = self.base.max(emitted);
        }
        let gap = at - self.end();
        let mut s = samples;
        let (mut filled, mut dropped) = (0, 0);
        if gap > TOLERANCE {
            // Source was silent (WASAPI loopback emits *nothing* when nothing plays) or
            // drifted slow: fill the hole so later audio stays aligned with video.
            self.buf.extend(std::iter::repeat_n(0.0, gap as usize * CH));
            filled = gap as u64;
        } else if gap < -TOLERANCE {
            let skip = ((-gap) as usize * CH).min(s.len());
            s = &s[skip..];
            dropped = (skip / CH) as u64;
        }
        self.buf.extend(s.iter().copied());
        (filled, dropped)
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

#[derive(Default)]
pub struct Controls {
    pub system_muted: AtomicBool,
    pub mic_muted: AtomicBool,
}

impl Controls {
    fn gains(&self, gains: &Gains) -> [f32; 2] {
        [
            if self.system_muted.load(Ordering::Relaxed) { 0.0 } else { gains.system },
            if self.mic_muted.load(Ordering::Relaxed) { 0.0 } else { gains.mic },
        ]
    }
}

#[derive(Default)]
pub struct Stats {
    pub corrections: AtomicU64,
    pub zero_filled: AtomicU64,
    pub overlap_dropped: AtomicU64,
}

fn emit_until<F>(upto: i64, final_flush: bool, tracks: &mut [Track; 2], emitted: &mut i64, gain: [f32; 2], sink: &mut F)
where
    F: FnMut(Vec<i16>, i64),
{
    let minimum = if final_flush { 1 } else { MIN_CHUNK };
    while upto - *emitted >= minimum {
        let n = (upto - *emitted).min(RATE) as usize;
        let mut mix = vec![0.0f32; n * CH];
        for (track, gain) in tracks.iter_mut().zip(gain) {
            track.pull_into(*emitted, n, &mut mix, gain);
        }
        let pcm = mix.iter().map(|v| (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).collect();
        sink(pcm, *emitted * 10_000_000 / RATE);
        *emitted += n as i64;
    }
}

/// Mixes system + mic onto the shared clock and hands PCM16 chunks to `sink(pcm, pts)`.
/// Ticks on its own timer (not on packet arrival), so a silent loopback never stalls the mic.
pub fn spawn<F>(
    rx: Receiver<Packet>,
    clock: Arc<Clock>,
    gains: Gains,
    controls: Arc<Controls>,
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
    mut sink: F,
) -> JoinHandle<()>
where
    F: FnMut(Vec<i16>, i64) + Send + 'static,
{
    thread::Builder::new()
        .name("oculus-mixer".into())
        .spawn(move || {
            #[cfg(debug_assertions)]
            crate::selftest::inject_delay("--stall-mixer-ms");
            let mut tracks = [Track::default(), Track::default()];
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
                    let (filled, dropped) = tracks[p.source as usize].push(hns_to_frames(pts), s, emitted);
                    if filled != 0 || dropped != 0 {
                        stats.corrections.fetch_add(1, Ordering::Relaxed);
                    }
                    stats.zero_filled.fetch_add(filled, Ordering::Relaxed);
                    stats.overlap_dropped.fetch_add(dropped, Ordering::Relaxed);
                }
            };

            loop {
                let disconnected = match rx.recv_timeout(Duration::from_millis(10)) {
                    Ok(p) => {
                        place(p, &mut tracks, emitted);
                        false
                    }
                    Err(RecvTimeoutError::Timeout) => false,
                    Err(RecvTimeoutError::Disconnected) => true,
                };
                while let Ok(p) = rx.try_recv() {
                    place(p, &mut tracks, emitted);
                }
                let final_flush = disconnected || stop.load(Ordering::Relaxed);
                let Some(now) = clock.now() else {
                    if final_flush {
                        break;
                    }
                    continue;
                };
                if final_flush {
                    // Final flush right up to "now" so audio doesn't end 100ms before video.
                    emit_until(hns_to_frames(now), true, &mut tracks, &mut emitted, controls.gains(&gains), &mut sink);
                    break;
                }
                emit_until(
                    hns_to_frames(now) - LATENCY,
                    false,
                    &mut tracks,
                    &mut emitted,
                    controls.gains(&gains),
                    &mut sink,
                );
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

    #[test]
    fn final_flush_emits_the_sub_chunk_tail() {
        let mut tracks = [Track::default(), Track::default()];
        tracks[0].push(0, &frames(1479, 0.5), 0);
        let mut emitted = 0;
        let mut chunks = Vec::new();
        let mut sink = |pcm: Vec<i16>, pts| chunks.push((pcm, pts));
        emit_until(1440, false, &mut tracks, &mut emitted, [1.0, 1.0], &mut sink);
        emit_until(1479, false, &mut tracks, &mut emitted, [1.0, 1.0], &mut sink);
        assert_eq!(emitted, 1440);
        emit_until(1479, true, &mut tracks, &mut emitted, [1.0, 1.0], &mut sink);
        assert_eq!(emitted, 1479);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[1].0.len(), 39 * CH);
        assert_eq!(chunks[1].1, 300_000);
        assert!(chunks[1].0.iter().all(|&v| v == (0.5 * i16::MAX as f32) as i16));
    }

    #[test]
    fn disconnected_capture_flushes_audio_without_a_stop_message() {
        let (tx, rx) = crate::queue::bounded(1);
        let clock = Arc::new(Clock::default());
        clock.video_pts(crate::util::qpc_now() - 1_000_000);
        drop(tx);
        let (out_tx, out_rx) = std::sync::mpsc::channel();
        let mixer = spawn(
            rx,
            clock,
            Gains { system: 1.0, mic: 1.0 },
            Arc::new(Controls::default()),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Stats::default()),
            move |pcm, pts| {
                out_tx.send((pcm, pts)).unwrap();
            },
        );
        mixer.join().unwrap();
        let chunks: Vec<_> = out_rx.into_iter().collect();
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].1, 0);
        assert!(chunks.iter().map(|(pcm, _)| pcm.len() / CH).sum::<usize>() >= 4800);
    }

    #[test]
    fn live_mute_drains_only_the_muted_track_and_unmute_keeps_timeline() {
        let controls = Controls::default();
        let gains = Gains { system: 1.0, mic: 1.0 };
        let mut tracks = [Track::default(), Track::default()];
        tracks[1].push(0, &vec![0.25; 480 * CH], 0);
        controls.mic_muted.store(true, Ordering::Relaxed);
        let mut emitted = 0;
        let mut output = Vec::new();
        emit_until(480, true, &mut tracks, &mut emitted, controls.gains(&gains), &mut |pcm, pts| {
            output.push((pcm, pts))
        });
        assert!(output[0].0.iter().all(|&sample| sample == 0));
        controls.mic_muted.store(false, Ordering::Relaxed);
        tracks[1].push(480, &vec![0.25; 480 * CH], emitted);
        emit_until(960, true, &mut tracks, &mut emitted, controls.gains(&gains), &mut |pcm, pts| {
            output.push((pcm, pts))
        });
        assert_eq!(output[1].1, 100_000);
        assert!(output[1].0.iter().all(|&sample| sample > 0));
    }
}
