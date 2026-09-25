use anyhow::{Result, anyhow};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

type Outcome<T> = Option<std::result::Result<T, Arc<str>>>;

/// Joins only overlapping operations. Entries disappear on completion or
/// cancellation; a later request always performs fresh I/O.
pub(super) struct Flights<T> {
    entries: Mutex<HashMap<String, watch::Receiver<Outcome<T>>>>,
}

impl<T> Default for Flights<T> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

struct Leader<'a, T> {
    flights: &'a Flights<T>,
    key: String,
    sender: watch::Sender<Outcome<T>>,
}
impl<T> Drop for Leader<'_, T> {
    fn drop(&mut self) {
        self.flights
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

impl<T: Clone + Send + Sync> Flights<T> {
    pub(super) async fn run(
        &self,
        key: String,
        operation: impl Future<Output = Result<T>> + Send,
    ) -> Result<T> {
        let (mut receiver, leader) = {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(receiver) = entries.get(&key) {
                (receiver.clone(), None)
            } else {
                let (sender, receiver) = watch::channel(None);
                entries.insert(key.clone(), receiver.clone());
                (
                    receiver,
                    Some(Leader {
                        flights: self,
                        key,
                        sender,
                    }),
                )
            }
        };
        if let Some(leader) = leader {
            let result = operation
                .await
                .map_err(|e| Arc::<str>::from(format!("{e:#}")));
            let _ = leader.sender.send(Some(result.clone()));
            return result.map_err(|error| anyhow!("{error}"));
        }
        loop {
            let result = receiver.borrow().clone();
            if let Some(result) = result {
                return result.map_err(|error| anyhow!("{error}"));
            }
            receiver
                .changed()
                .await
                .map_err(|_| anyhow!("shared provider request was cancelled; retry"))?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn overlaps_share_a_result_but_later_calls_never_cache_it() {
        let flights = Flights::default();
        let count = AtomicUsize::new(0);
        let operation = || async {
            count.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(42)
        };
        let (a, b) = tokio::join!(
            flights.run("same".into(), operation()),
            flights.run("same".into(), operation())
        );
        assert_eq!(a.unwrap(), b.unwrap());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        flights.run("same".into(), operation()).await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert!(flights.entries.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancelled_leader_releases_followers_and_removes_entry() {
        let flights = Flights::<()>::default();
        let first = flights.run("same".into(), std::future::pending());
        let mut first = Box::pin(first);
        assert!(futures_util::poll!(&mut first).is_pending());
        let second = flights.run("same".into(), async { Ok(()) });
        let mut second = Box::pin(second);
        assert!(futures_util::poll!(&mut second).is_pending());
        drop(first);
        assert!(second.await.is_err());
        flights.run("same".into(), async { Ok(()) }).await.unwrap();
    }
}
