//! 失败限流：同一键（用户名）连续失败 N 次后锁定一段时间；成功即清零。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    max_failures: u32,
    lockout: Duration,
    state: Mutex<HashMap<String, (u32, Option<Instant>)>>,
}

impl RateLimiter {
    pub fn new(max_failures: u32, lockout: Duration) -> Self {
        Self { max_failures, lockout, state: Mutex::new(HashMap::new()) }
    }

    /// 仍在锁定期时返回剩余秒数。
    pub fn locked(&self, key: &str) -> Option<u64> {
        let mut state = self.state.lock().expect("ratelimit poisoned");
        let entry = state.get_mut(key)?;
        match entry.1 {
            Some(until) if until > Instant::now() => Some((until - Instant::now()).as_secs().max(1)),
            Some(_) => {
                *entry = (0, None);
                None
            }
            None => None,
        }
    }

    pub fn failure(&self, key: &str) {
        let mut state = self.state.lock().expect("ratelimit poisoned");
        let entry = state.entry(key.to_owned()).or_insert((0, None));
        entry.0 += 1;
        if entry.0 >= self.max_failures {
            entry.1 = Some(Instant::now() + self.lockout);
        }
    }

    pub fn success(&self, key: &str) {
        self.state.lock().expect("ratelimit poisoned").remove(key);
    }
}

/// 滑动窗口限频：每个键在 `window` 内最多 `max` 次。
pub struct Throttle {
    max: usize,
    window: Duration,
    hits: Mutex<HashMap<String, std::collections::VecDeque<Instant>>>,
}

impl Throttle {
    pub fn new(max: usize, window: Duration) -> Self {
        Self { max, window, hits: Mutex::new(HashMap::new()) }
    }

    /// 记一次；超过频率时返回需要等待的秒数。
    pub fn hit(&self, key: &str) -> Result<(), u64> {
        let now = Instant::now();
        let mut hits = self.hits.lock().expect("throttle poisoned");
        // 顺带清掉早已过期的键，防止 map 无限增长
        if hits.len() > 1024 {
            hits.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < self.window));
        }
        let q = hits.entry(key.to_owned()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= self.window) {
            q.pop_front();
        }
        if q.len() >= self.max {
            let wait = self.window.saturating_sub(now.duration_since(*q.front().expect("non-empty")));
            return Err(wait.as_secs().max(1));
        }
        q.push_back(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_limits_per_key() {
        let t = Throttle::new(2, Duration::from_secs(60));
        assert!(t.hit("a").is_ok());
        assert!(t.hit("a").is_ok());
        assert!(t.hit("a").is_err());
        assert!(t.hit("b").is_ok());
    }

    #[test]
    fn locks_after_max_failures_and_success_resets() {
        let rl = RateLimiter::new(3, Duration::from_secs(60));
        rl.failure("a");
        rl.failure("a");
        assert!(rl.locked("a").is_none());
        rl.success("a");
        rl.failure("a");
        rl.failure("a");
        assert!(rl.locked("a").is_none(), "success reset the count");
        rl.failure("a");
        assert!(rl.locked("a").is_some());
        assert!(rl.locked("b").is_none());
    }
}
