use std::sync::Mutex;

use crate::util::qpc_now;

/// Recording timeline shared by video and audio. Everything is QPC (100ns) in, PTS out.
///
/// `start` latches on the first WGC frame, not on the Record click: WGC needs 15–40ms to
/// deliver frame 0 while WASAPI is already producing packets, so latching early would put
/// audio ahead of a black first frame.
#[derive(Default)]
pub struct Clock {
    s: Mutex<State>,
}

#[derive(Default)]
struct State {
    start: Option<i64>,
    paused_total: i64,
    pause_start: Option<i64>,
    stopped_at: Option<i64>,
    /// Full pause history: late packets can precede several rapid pause/resume cycles.
    pauses: Vec<(i64, i64)>,
}

impl Clock {
    /// PTS for a video frame, latching the start on the first one. None while paused.
    pub fn video_pts(&self, qpc: i64) -> Option<i64> {
        let mut s = self.s.lock().unwrap();
        if s.start.is_none() {
            // Do not latch frame zero inside a pause. Otherwise resume subtracts time
            // from before the latch and the first recorded timestamps can be negative.
            if s.pause_start.is_some() || s.stopped_at.is_some() {
                return None;
            }
            s.start = Some(qpc);
        }
        s.to_pts(qpc)
    }

    /// PTS for an audio packet. None before the first video frame and while paused, which
    /// is how pre-roll audio gets discarded.
    pub fn pts(&self, qpc: i64) -> Option<i64> {
        self.s.lock().unwrap().to_pts(qpc)
    }

    /// Current position on the timeline (frozen while paused or after Stop).
    pub fn now(&self) -> Option<i64> {
        let s = self.s.lock().unwrap();
        let start = s.start?;
        Some(s.pause_start.or(s.stopped_at).unwrap_or_else(qpc_now) - start - s.paused_total)
    }

    pub fn start(&self) -> Option<i64> {
        self.s.lock().unwrap().start
    }

    pub fn paused(&self) -> bool {
        self.s.lock().unwrap().pause_start.is_some()
    }

    pub fn pause(&self) {
        let mut s = self.s.lock().unwrap();
        if s.pause_start.is_none() && s.stopped_at.is_none() {
            s.pause_start = Some(qpc_now());
        }
    }

    pub fn resume(&self) {
        let mut s = self.s.lock().unwrap();
        if s.stopped_at.is_some() {
            return;
        }
        if let Some(p) = s.pause_start.take() {
            let now = qpc_now();
            // Paused before frame 0 arrived: nothing to subtract, the start hasn't happened.
            if s.start.is_some() {
                s.paused_total += now - p;
                s.pauses.push((p, now));
            }
        }
    }

    /// Freeze at the first Stop request, before capture/device teardown can take time.
    /// Late audio captured before this boundary can still be placed and flushed.
    pub fn finish(&self) {
        let mut s = self.s.lock().unwrap();
        if s.stopped_at.is_none() {
            s.stopped_at = Some(qpc_now());
        }
    }
}

impl State {
    fn to_pts(&self, qpc: i64) -> Option<i64> {
        let start = self.start?;
        if qpc < start || self.pause_start.is_some_and(|p| qpc >= p) || self.stopped_at.is_some_and(|p| qpc >= p) {
            return None;
        }
        let mut paused = 0;
        for &(a, b) in &self.pauses {
            if qpc < a {
                break;
            }
            if qpc < b {
                return None;
            }
            paused += b - a;
        }
        Some(qpc - start - paused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::mixer::Track;

    #[test]
    fn stop_freezes_flush_endpoint_but_accepts_late_pre_stop_audio() {
        let clock = Clock { s: Mutex::new(State { start: Some(100), stopped_at: Some(500), ..Default::default() }) };
        assert_eq!(clock.now(), Some(400));
        assert_eq!(clock.pts(499), Some(399));
        assert_eq!(clock.pts(500), None);
        clock.finish();
        clock.pause();
        clock.resume();
        assert_eq!(clock.now(), Some(400));
    }

    #[test]
    fn stop_during_pause_keeps_pause_endpoint_and_stop_before_start_never_latches() {
        let clock = Clock {
            s: Mutex::new(State {
                start: Some(100),
                pause_start: Some(300),
                stopped_at: Some(500),
                ..Default::default()
            }),
        };
        clock.resume();
        assert_eq!(clock.now(), Some(200));
        assert_eq!(clock.pts(299), Some(199));
        assert_eq!(clock.pts(300), None);
        let unstarted = Clock::default();
        unstarted.finish();
        assert_eq!(unstarted.video_pts(qpc_now()), None);
        assert_eq!(unstarted.now(), None);
    }

    #[test]
    fn late_audio_uses_all_pauses_without_negative_pts() {
        let state = State {
            start: Some(1_000_000),
            paused_total: 400_000,
            pauses: vec![(1_200_000, 1_400_000), (1_600_000, 1_800_000)],
            ..Default::default()
        };
        assert_eq!(state.to_pts(900_000), None);
        assert_eq!(state.to_pts(1_000_000), Some(0));
        assert_eq!(state.to_pts(1_100_000), Some(100_000));
        assert_eq!(state.to_pts(1_200_000), None);
        assert_eq!(state.to_pts(1_399_999), None);
        assert_eq!(state.to_pts(1_400_000), Some(200_000));
        assert_eq!(state.to_pts(1_500_000), Some(300_000));
        assert_eq!(state.to_pts(1_700_000), None);
        assert_eq!(state.to_pts(1_900_000), Some(500_000));

        // The late packet from before both pauses must land at 10ms in the mixer,
        // rather than getting a negative timestamp and being discarded.
        let pts = state.to_pts(1_100_000).unwrap();
        let at = pts * 48_000 / 10_000_000;
        let mut track = Track::default();
        track.push(at, &vec![0.5; 480 * 2], 480);
        let mut mixed = vec![0.0; 480 * 2];
        track.pull_into(480, 480, &mut mixed, 1.0);
        assert!(mixed.iter().all(|&v| v == 0.5));
    }

    #[test]
    fn pause_before_first_frame_does_not_latch_start() {
        let clock = Clock::default();
        clock.pause();
        assert_eq!(clock.video_pts(qpc_now()), None);
        assert_eq!(clock.start(), None);
        clock.resume();
        assert_eq!(clock.video_pts(qpc_now()), Some(0));
    }

    #[test]
    fn current_pause_rejects_only_packets_captured_during_it() {
        let state = State { start: Some(100), pause_start: Some(200), ..Default::default() };
        assert_eq!(state.to_pts(150), Some(50));
        assert_eq!(state.to_pts(200), None);
    }
}
