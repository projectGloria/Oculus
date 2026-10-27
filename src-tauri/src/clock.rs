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
    /// Most recent completed pause, for audio packets that arrive late (up to the mixer's
    /// latency) and were captured before it.
    last_pause: Option<(i64, i64)>,
}

impl Clock {
    /// PTS for a video frame, latching the start on the first one. None while paused.
    pub fn video_pts(&self, qpc: i64) -> Option<i64> {
        let mut s = self.s.lock().unwrap();
        if s.start.is_none() {
            s.start = Some(qpc);
        }
        s.to_pts(qpc)
    }

    /// PTS for an audio packet. None before the first video frame and while paused, which
    /// is how pre-roll audio gets discarded.
    pub fn pts(&self, qpc: i64) -> Option<i64> {
        self.s.lock().unwrap().to_pts(qpc)
    }

    /// Current position on the timeline (frozen while paused).
    pub fn now(&self) -> Option<i64> {
        let s = self.s.lock().unwrap();
        let start = s.start?;
        Some(s.pause_start.unwrap_or_else(qpc_now) - start - s.paused_total)
    }

    pub fn start(&self) -> Option<i64> {
        self.s.lock().unwrap().start
    }

    pub fn paused(&self) -> bool {
        self.s.lock().unwrap().pause_start.is_some()
    }

    pub fn pause(&self) {
        let mut s = self.s.lock().unwrap();
        if s.pause_start.is_none() {
            s.pause_start = Some(qpc_now());
        }
    }

    pub fn resume(&self) {
        let mut s = self.s.lock().unwrap();
        if let Some(p) = s.pause_start.take() {
            let now = qpc_now();
            // Paused before frame 0 arrived: nothing to subtract, the start hasn't happened.
            if s.start.is_some() {
                s.paused_total += now - p;
                s.last_pause = Some((p, now));
            }
        }
    }
}

impl State {
    fn to_pts(&self, qpc: i64) -> Option<i64> {
        let start = self.start?;
        if qpc < start || self.pause_start.is_some_and(|p| qpc >= p) {
            return None;
        }
        let mut paused = self.paused_total;
        if let Some((a, b)) = self.last_pause {
            if qpc >= a && qpc < b {
                return None;
            }
            if qpc < a {
                paused -= b - a;
            }
        }
        Some(qpc - start - paused)
    }
}
