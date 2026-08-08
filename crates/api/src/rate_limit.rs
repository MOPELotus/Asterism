use std::{
    collections::{HashMap, VecDeque},
    hash::Hash,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const DEFAULT_PRINCIPAL_ATTEMPTS: usize = 5;
const DEFAULT_IP_ATTEMPTS: usize = 30;
const DEFAULT_WINDOW: Duration = Duration::from_mins(1);
const MAX_TRACKED_PRINCIPALS: usize = 4096;
const MAX_TRACKED_IPS: usize = 1024;

#[derive(Clone, Debug)]
pub(crate) struct LoginRateLimiter {
    inner: Arc<Mutex<LoginRateState>>,
    principal_attempts: usize,
    ip_attempts: usize,
    window: Duration,
}

#[derive(Debug, Default)]
struct LoginRateState {
    by_principal: HashMap<PrincipalKey, VecDeque<Instant>>,
    by_ip: HashMap<IpAddr, VecDeque<Instant>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PrincipalKey {
    ip: IpAddr,
    username: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RateLimitExceeded {
    pub(crate) retry_after_seconds: u64,
}

impl LoginRateLimiter {
    #[cfg(test)]
    pub(crate) fn new(principal_attempts: usize, ip_attempts: usize, window: Duration) -> Self {
        assert!(principal_attempts > 0);
        assert!(ip_attempts >= principal_attempts);
        assert!(!window.is_zero());
        Self {
            inner: Arc::new(Mutex::new(LoginRateState::default())),
            principal_attempts,
            ip_attempts,
            window,
        }
    }

    pub(crate) fn check_and_record(
        &self,
        ip: IpAddr,
        username: &str,
    ) -> Result<(), RateLimitExceeded> {
        self.check_and_record_at(ip, username, Instant::now())
    }

    fn check_and_record_at(
        &self,
        ip: IpAddr,
        username: &str,
        now: Instant,
    ) -> Result<(), RateLimitExceeded> {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune_map(&mut state.by_principal, now, self.window);
        prune_map(&mut state.by_ip, now, self.window);

        let key = PrincipalKey {
            ip,
            username: username.to_ascii_lowercase(),
        };
        if let Some(retry_after_seconds) = state
            .by_principal
            .get(&key)
            .filter(|attempts| attempts.len() >= self.principal_attempts)
            .and_then(|attempts| retry_after(attempts, now, self.window))
        {
            return Err(RateLimitExceeded {
                retry_after_seconds,
            });
        }
        if let Some(retry_after_seconds) = state
            .by_ip
            .get(&ip)
            .filter(|attempts| attempts.len() >= self.ip_attempts)
            .and_then(|attempts| retry_after(attempts, now, self.window))
        {
            return Err(RateLimitExceeded {
                retry_after_seconds,
            });
        }
        if (!state.by_principal.contains_key(&key)
            && state.by_principal.len() >= MAX_TRACKED_PRINCIPALS)
            || (!state.by_ip.contains_key(&ip) && state.by_ip.len() >= MAX_TRACKED_IPS)
        {
            return Err(RateLimitExceeded {
                retry_after_seconds: self.window.as_secs().max(1),
            });
        }

        state.by_principal.entry(key).or_default().push_back(now);
        state.by_ip.entry(ip).or_default().push_back(now);
        Ok(())
    }
}

impl Default for LoginRateLimiter {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LoginRateState::default())),
            principal_attempts: DEFAULT_PRINCIPAL_ATTEMPTS,
            ip_attempts: DEFAULT_IP_ATTEMPTS,
            window: DEFAULT_WINDOW,
        }
    }
}

fn prune_map<K: Eq + Hash>(
    map: &mut HashMap<K, VecDeque<Instant>>,
    now: Instant,
    window: Duration,
) {
    map.retain(|_, attempts| {
        while attempts
            .front()
            .is_some_and(|attempt| now.duration_since(*attempt) >= window)
        {
            attempts.pop_front();
        }
        !attempts.is_empty()
    });
}

fn retry_after(attempts: &VecDeque<Instant>, now: Instant, window: Duration) -> Option<u64> {
    let oldest = *attempts.front()?;
    let remaining = window.saturating_sub(now.duration_since(oldest));
    Some(
        remaining
            .as_secs()
            .saturating_add(u64::from(remaining.subsec_nanos() > 0))
            .max(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_a_principal_and_releases_it_after_the_window() {
        let limiter = LoginRateLimiter::new(2, 4, Duration::from_mins(1));
        let ip = IpAddr::from([127, 0, 0, 1]);
        let start = Instant::now();
        assert!(limiter.check_and_record_at(ip, "Master", start).is_ok());
        assert!(
            limiter
                .check_and_record_at(ip, "master", start + Duration::from_secs(1))
                .is_ok()
        );
        let rejection = limiter
            .check_and_record_at(ip, "MASTER", start + Duration::from_secs(2))
            .unwrap_err();
        assert_eq!(rejection.retry_after_seconds, 58);
        assert!(
            limiter
                .check_and_record_at(ip, "master", start + Duration::from_mins(1))
                .is_ok()
        );
    }

    #[test]
    fn limits_total_attempts_from_one_ip() {
        let limiter = LoginRateLimiter::new(2, 3, Duration::from_mins(1));
        let ip = IpAddr::from([127, 0, 0, 1]);
        let start = Instant::now();
        for username in ["a", "b", "c"] {
            assert!(limiter.check_and_record_at(ip, username, start).is_ok());
        }
        assert!(limiter.check_and_record_at(ip, "d", start).is_err());
    }
}
