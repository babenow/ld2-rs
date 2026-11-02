//! Chaos-режим мока: детерминированные случайные обрывы соединений
//! (seed → xorshift64; `docs/tz.md` §5, PLAN-M1 T3).

#[derive(Clone)]
pub struct Chaos {
    enabled: bool,
    /// Вероятность обрыва = 1 / every (после каждого обработанного сообщения).
    every: u32,
    state: u64,
}

impl Chaos {
    pub fn new(enabled: bool, every: u32, seed: u64) -> Self {
        Self {
            enabled,
            every: every.max(1),
            state: seed | 1, // нулевой seed вырождается
        }
    }

    pub fn disabled() -> Self {
        Self::new(false, 1, 1)
    }

    /// Обрывить ли соединение после обработки сообщения.
    pub fn should_drop(&mut self) -> bool {
        if !self.enabled {
            return false;
        }
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state.is_multiple_of(u64::from(self.every))
    }
}

impl Default for Chaos {
    fn default() -> Self {
        Self::disabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_never_drops() {
        let mut c = Chaos::disabled();
        assert!((0..100).all(|_| !c.should_drop()));
    }

    #[test]
    fn seeded_sequence_is_reproducible() {
        let run = |seed| -> Vec<bool> {
            let mut c = Chaos::new(true, 3, seed);
            (0..20).map(|_| c.should_drop()).collect()
        };
        assert_eq!(
            run(42),
            run(42),
            "одинаковый seed → одинаковая последовательность"
        );
        assert!(
            run(42).iter().any(|x| *x),
            "при every=3 обрывы должны случаться"
        );
        assert_ne!(run(42), run(7), "разные seed — разные последовательности");
    }
}
