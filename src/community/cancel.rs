//! 下载取消令牌（与运行时无关，WASM 里同样可用）。

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// 等到被取消。
    pub async fn cancelled(&self) {
        loop {
            let notified = self.inner.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }

    /// 跑一个 future，先被取消就返回 `None`（future 随即被丢弃）。
    pub async fn run<F: Future>(&self, future: F) -> Option<F::Output> {
        use futures::future::{Either, select};
        if self.is_cancelled() {
            return None;
        }
        let future = std::pin::pin!(future);
        let cancelled = std::pin::pin!(self.cancelled());
        match select(cancelled, future).await {
            Either::Left(_) => None,
            Either::Right((output, _)) => Some(output),
        }
    }
}
