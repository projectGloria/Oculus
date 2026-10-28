//! Bounded, nonblocking producer queues. Stop/error requests use a separate channel so
//! a full media queue cannot block UI commands or capture callbacks.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError, TrySendError};
use std::sync::Arc;
use std::time::Duration;

#[derive(Default)]
pub struct Metrics {
    queued: AtomicUsize,
    pub peak: AtomicUsize,
    pub full: AtomicU64,
}

struct Entry<T> {
    value: Option<T>,
    metrics: Arc<Metrics>,
}

impl<T> Entry<T> {
    fn take(mut self) -> T {
        self.value.take().unwrap()
    }
}

impl<T> Drop for Entry<T> {
    fn drop(&mut self) {
        self.metrics.queued.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct Sender<T> {
    sender: mpsc::SyncSender<Entry<T>>,
    metrics: Arc<Metrics>,
    capacity: usize,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self { sender: self.sender.clone(), metrics: self.metrics.clone(), capacity: self.capacity }
    }
}

pub struct Receiver<T> {
    receiver: mpsc::Receiver<Entry<T>>,
}

pub fn bounded<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    assert!(capacity > 0);
    let (sender, receiver) = mpsc::sync_channel(capacity);
    let metrics = Arc::new(Metrics::default());
    (Sender { sender, metrics, capacity }, Receiver { receiver })
}

impl<T> Sender<T> {
    pub fn metrics(&self) -> Arc<Metrics> {
        self.metrics.clone()
    }

    pub fn try_send(&self, value: T) -> Result<(), TrySendError<T>> {
        // Reserve the count before publishing: a fast receiver must never decrement an
        // uncounted entry. Entry::drop also handles disconnection and receiver teardown.
        let depth = self.metrics.queued.fetch_add(1, Ordering::Relaxed) + 1;
        let entry = Entry { value: Some(value), metrics: self.metrics.clone() };
        match self.sender.try_send(entry) {
            Ok(()) => {
                // Concurrent producer reservations can overestimate instantaneous depth;
                // report a conservative high-water mark capped at the actual capacity.
                self.metrics.peak.fetch_max(depth.min(self.capacity), Ordering::Relaxed);
                Ok(())
            }
            Err(TrySendError::Full(entry)) => {
                if self.metrics.full.fetch_add(1, Ordering::Relaxed) == 0 {
                    tracing::warn!(capacity = self.capacity, "Recording queue rejected its first packet");
                }
                Err(TrySendError::Full(entry.take()))
            }
            Err(TrySendError::Disconnected(entry)) => Err(TrySendError::Disconnected(entry.take())),
        }
    }
}

impl<T> Receiver<T> {
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        self.receiver.recv_timeout(timeout).map(Entry::take)
    }

    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.receiver.try_recv().map(Entry::take)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_queue_rejects_immediately_and_keeps_accepted_items() {
        let (tx, rx) = bounded(2);
        tx.try_send(1).unwrap();
        tx.try_send(2).unwrap();
        assert!(matches!(tx.try_send(3), Err(TrySendError::Full(3))));
        assert_eq!(tx.metrics().full.load(Ordering::Relaxed), 1);
        assert_eq!(rx.try_recv().unwrap(), 1);
        assert_eq!(rx.try_recv().unwrap(), 2);
        assert_eq!(tx.metrics().queued.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn concurrent_producers_cannot_exceed_capacity() {
        let (tx, rx) = bounded(4);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let tx = tx.clone();
                scope.spawn(move || {
                    for n in 0..100 {
                        let _ = tx.try_send(n);
                    }
                });
            }
        });
        assert_eq!(tx.metrics().queued.load(Ordering::Relaxed), 4);
        assert_eq!(tx.metrics().peak.load(Ordering::Relaxed), 4);
        assert_eq!(tx.metrics().full.load(Ordering::Relaxed), 1596);
        drop(rx);
        assert_eq!(tx.metrics().queued.load(Ordering::Relaxed), 0);
        assert!(matches!(tx.try_send(9), Err(TrySendError::Disconnected(9))));
        assert_eq!(tx.metrics().queued.load(Ordering::Relaxed), 0);
    }
}
