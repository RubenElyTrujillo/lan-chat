use std::time::Duration;

/// Exponential backoff with full jitter: 1s, 2s, 4s ... capped at 30s.
/// `reset()` on a successful session. The rng closure is injectable so
/// tests are deterministic.
pub struct ReconnectBackoff {
    attempt: u32,
}

const BASE_MS: u64 = 1_000;
const CAP_MS: u64 = 30_000;

impl ReconnectBackoff {
    pub fn new() -> Self {
        Self { attempt: 0 }
    }

    /// Pure exponential component: 1000, 2000, 4000 ... capped at 30_000 ms.
    pub fn exponential_ms(attempt: u32) -> u64 {
        BASE_MS.saturating_mul(1u64 << attempt.min(31)).min(CAP_MS)
    }

    /// Full-jitter style: bounded delay in [1, exponential_ms(attempt)] ms.
    /// Sampled as `rng() % (upper + 1)` then clamped to the 1 ms floor, so
    /// the distribution is bounded but not perfectly uniform (slight modulo
    /// bias); the floor deforms the lowest roll only.
    pub fn next_delay_with(&mut self, rng: &mut impl FnMut() -> u64) -> Duration {
        let upper = Self::exponential_ms(self.attempt);
        self.attempt = self.attempt.saturating_add(1);
        let roll = rng() % (upper + 1);
        Duration::from_millis(roll.max(1).min(upper))
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Draws the next delay with real OS entropy. If entropy is unavailable,
    /// returns the deterministic exponential delay for the current attempt —
    /// conservative (seconds, never the 1 ms floor), so an entropy failure
    /// can never degrade into a busy reconnect loop.
    pub fn next_delay(&mut self) -> Duration {
        self.next_delay_with_entropy(os_entropy_u64())
    }

    /// Testable core of [`next_delay`]: `None` entropy falls back to the full
    /// exponential delay for the current attempt (still bounded by the cap).
    pub fn next_delay_with_entropy(&mut self, entropy: Option<u64>) -> Duration {
        match entropy {
            Some(v) => self.next_delay_with(&mut move || v),
            None => {
                let delay = Duration::from_millis(Self::exponential_ms(self.attempt));
                self.attempt = self.attempt.saturating_add(1);
                delay
            }
        }
    }
}

fn os_entropy_u64() -> Option<u64> {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).ok()?;
    Some(u64::from_le_bytes(b))
}

impl Default for ReconnectBackoff {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn exponential_sequence_is_exact_and_capped() {
        assert_eq!(ReconnectBackoff::exponential_ms(0), 1_000);
        assert_eq!(ReconnectBackoff::exponential_ms(1), 2_000);
        assert_eq!(ReconnectBackoff::exponential_ms(2), 4_000);
        assert_eq!(ReconnectBackoff::exponential_ms(5), 30_000); // capped
        assert_eq!(ReconnectBackoff::exponential_ms(50), 30_000);
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let mut bo = ReconnectBackoff::new();
        let d = bo.next_delay_with(&mut || 0u64);
        assert_eq!(d, Duration::from_millis(1)); // clamped to the 1 ms floor
        let mut bo = ReconnectBackoff::new();
        let d = bo.next_delay_with(&mut || u64::MAX);
        assert!(d >= Duration::from_millis(1));
        assert!(d <= Duration::from_millis(1_000));
    }

    #[test]
    fn entropy_failure_falls_back_to_conservative_exponential() {
        // Entropy failure must never produce the 1 ms floor or a busy loop:
        // the full exponential delay (capped) is the safe conservative path.
        let mut bo = ReconnectBackoff::new();
        assert_eq!(
            bo.next_delay_with_entropy(None),
            Duration::from_millis(1_000)
        );
        assert_eq!(
            bo.next_delay_with_entropy(None),
            Duration::from_millis(2_000)
        );
    }

    #[test]
    fn entropy_path_stays_bounded() {
        let mut bo = ReconnectBackoff::new();
        let d = bo.next_delay();
        assert!(d >= Duration::from_millis(1));
        assert!(d <= Duration::from_millis(1_000));
    }

    #[test]
    fn attempts_advance_up_to_cap_then_reset() {
        let mut bo = ReconnectBackoff::new();
        let mut seq: u64 = 0;
        let mut rng = move || {
            seq += 1;
            seq
        };
        for attempt in 0..8u32 {
            let d = bo.next_delay_with(&mut rng);
            let upper = ReconnectBackoff::exponential_ms(attempt);
            assert!(d >= Duration::from_millis(1));
            assert!(d <= Duration::from_millis(upper));
        }
        // Beyond the cap the upper bound stays 30 s.
        let d = bo.next_delay_with(&mut rng);
        assert!(d <= std::time::Duration::from_secs(30));
        bo.reset();
        let d = bo.next_delay_with(&mut rng);
        assert!(d <= std::time::Duration::from_millis(1_000));
    }
}
