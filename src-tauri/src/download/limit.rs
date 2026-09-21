use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// 等待空位时的兜底轮询间隔。`notify_waiters` 只唤醒当时已登记的等待者，
/// 登记与唤醒之间有窗口，靠这个兜底保证不会有人一直睡着。
const RECHECK_INTERVAL: Duration = Duration::from_millis(200);

/// 全局连接数闸门：所有任务的所有通道共用一个上限。
/// 通道领到分片后先在这里领位，下完（或出错让出）再还位；
/// 上限随时可改——调大立刻放行排队的通道，调小则等已连上的通道陆续还位后生效。
pub struct ConnectionGate {
    limit: AtomicUsize,
    active: Mutex<usize>,
    idle: Notify,
}

/// 占位凭据：离开作用域即还位。
pub struct GatePermit(Arc<ConnectionGate>);

impl Drop for GatePermit {
    fn drop(&mut self) {
        *self.0.active.lock().unwrap() -= 1;
        self.0.idle.notify_waiters();
    }
}

impl ConnectionGate {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(ConnectionGate {
            limit: AtomicUsize::new(limit.max(1)),
            active: Mutex::new(0),
            idle: Notify::new(),
        })
    }

    pub fn limit(&self) -> usize {
        self.limit.load(Ordering::SeqCst)
    }

    /// 已占用的连接数，验证与调试用。
    pub fn active(&self) -> usize {
        *self.active.lock().unwrap()
    }

    pub fn set_limit(&self, limit: usize) {
        self.limit.store(limit.max(1), Ordering::SeqCst);
        self.idle.notify_waiters();
    }

    /// 等一个空位；等待期间被取消返回 None，调用方按「已停止」收尾。
    pub async fn acquire(self: &Arc<Self>, cancel: &CancellationToken) -> Option<GatePermit> {
        loop {
            let limit = self.limit();
            {
                let mut active = self.active.lock().unwrap();
                if *active < limit {
                    *active += 1;
                    return Some(GatePermit(Arc::clone(self)));
                }
            }
            tokio::select! {
                _ = cancel.cancelled() => return None,
                _ = self.idle.notified() => {}
                _ = tokio::time::sleep(RECHECK_INTERVAL) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn respects_limit_and_releases() {
        let gate = ConnectionGate::new(2);
        let cancel = CancellationToken::new();
        let first = gate.acquire(&cancel).await.unwrap();
        let second = gate.acquire(&cancel).await.unwrap();
        assert_eq!(gate.active(), 2);

        // 满员时第三个要等位
        assert!(tokio::time::timeout(Duration::from_millis(80), gate.acquire(&cancel))
            .await
            .is_err());

        drop(first);
        let third = tokio::time::timeout(Duration::from_millis(500), gate.acquire(&cancel))
            .await
            .expect("还位后应能领到")
            .unwrap();
        assert_eq!(gate.active(), 2);

        // 上限调小：已占用的不受影响，之后新的要等
        gate.set_limit(1);
        assert!(tokio::time::timeout(Duration::from_millis(80), gate.acquire(&cancel))
            .await
            .is_err());
        drop(second);
        drop(third);
        assert_eq!(gate.active(), 0);
    }

    #[tokio::test]
    async fn cancel_while_waiting_gives_none() {
        let gate = ConnectionGate::new(1);
        let cancel = CancellationToken::new();
        let _held = gate.acquire(&cancel).await.unwrap();
        let waiting = tokio::spawn({
            let gate = Arc::clone(&gate);
            let cancel = cancel.clone();
            async move { gate.acquire(&cancel).await.is_none() }
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.cancel();
        assert!(waiting.await.unwrap());
    }
}
