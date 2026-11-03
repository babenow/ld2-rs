//! Backoff переподключения: 0.5 с → 30 с, экспоненциальный рост ±20% jitter
//! (ТЗ §6.3, PLAN-M1 T2). Детерминирован тестируемо (без rand-зависимости).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Задержка перед попыткой `attempt` (1-based): `base·2^(attempt−1)` с
/// jitter ±20%, всегда ≤ `max`.
pub(super) fn delay(base: Duration, max: Duration, attempt: u32) -> Duration {
    let exp = attempt.saturating_sub(1).min(20);
    let base_ms = base.as_millis() as u64;
    let raw = base_ms.saturating_mul(1u64 << exp);
    let capped = raw.min(max.as_millis() as u64);
    with_jitter(capped, salt(attempt)).min(max)
}

/// ±20% вокруг `ms`.
fn with_jitter(ms: u64, salt: u64) -> Duration {
    let frac = (splitmix64(salt) % 1000) as f64 / 1000.0; // 0.0 … 0.999
    let factor = 0.8 + 0.4 * frac; // 0.8 … 1.2
    Duration::from_millis((ms as f64 * factor) as u64)
}

fn salt(attempt: u32) -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos ^ (u64::from(attempt) << 32)
}

/// Простой splitmix64 — достаточно для jitter (не для криптографии).
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: Duration = Duration::from_millis(500);
    const MAX: Duration = Duration::from_secs(30);

    #[test]
    fn grows_exponentially_and_caps() {
        let d1 = delay(BASE, MAX, 1);
        let d3 = delay(BASE, MAX, 3);
        let d10 = delay(BASE, MAX, 10);
        let d100 = delay(BASE, MAX, 100);
        // Рост (с jitter-допуском: d1 ≤ 600мс, d3 ≥ 1.6с)
        assert!(d1 <= Duration::from_millis(600), "{d1:?}");
        assert!(d3 >= Duration::from_millis(1600), "{d3:?}");
        assert!(d3 > d1);
        // Потолок 30 с соблюдается всегда (T2: восстановление ≤ 30 с)
        assert!(d10 <= MAX, "{d10:?}");
        assert!(d100 <= MAX, "{d100:?}");
    }

    #[test]
    fn jitter_within_20_percent() {
        for attempt in 1..=6 {
            let d = delay(BASE, MAX, attempt);
            let nominal_ms = (500u64 << (attempt - 1)).min(30_000);
            let lo = (nominal_ms as f64 * 0.8) as u64;
            let hi = (nominal_ms as f64 * 1.2) as u64;
            let d_ms = d.as_millis() as u64;
            assert!(
                d_ms >= lo.saturating_sub(1) && d_ms <= hi + 1,
                "attempt {attempt}: {d_ms}ms не в [{lo}, {hi}]"
            );
        }
    }

    #[test]
    fn first_attempt_is_quick() {
        // Первый ретрай должен быть ~0.5с, иначе DoD «≤ 30 с» хрупкий.
        assert!(delay(BASE, MAX, 1) <= Duration::from_millis(600));
    }
}
