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

#[cfg(test)]
mod tests {
    use super::*;

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
