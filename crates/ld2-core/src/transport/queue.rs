//! Очередь неотвеченных команд: `seq` → pending, таймауты и идемпотентный
//! повтор ×1 (`docs/protocol.md` §2.4: окно 128, ретраи только идемпотентных).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ld2_protocol::Envelope;

/// Максимум неотвеченных команд (зеркало серверного окна §2.4).
pub(super) const MAX_PENDING: usize = 128;

#[derive(Debug, Clone)]
pub(super) struct PendingCommand {
    pub envelope: Envelope,
    pub idempotent: bool,
    pub sent_at: Instant,
    /// Сколько раз отправлялось (1 = первая отправка).
    pub attempts: u8,
}

/// Результат сканирования таймаутов.
#[derive(Debug, Default)]
pub(super) struct Timeouts {
    /// Идемпотентные — отправить повторно тем же `seq`.
    pub retry: Vec<Envelope>,
    /// Просроченные — сообщить приложению `CommandTimeout`.
    pub expired: Vec<(u32, String)>,
}

#[derive(Debug, Default)]
pub(super) struct PendingQueue {
    map: HashMap<u32, PendingCommand>,
}

impl PendingQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, envelope: Envelope, idempotent: bool) {
        let Some(seq) = envelope.seq else {
            debug_assert!(false, "команда без seq не должна попадать в очередь");
            return;
        };
        if self.map.len() >= MAX_PENDING {
            // Не роняем клиента: старейшая запись вытесняется (её ack будет
            // «неизвестным» и просто залогируется).
            if let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, p)| p.sent_at)
                .map(|(k, _)| *k)
            {
                self.map.remove(&oldest);
            }
        }
        self.map.insert(
            seq,
            PendingCommand {
                envelope,
                idempotent,
                sent_at: Instant::now(),
                attempts: 1,
            },
        );
    }

    /// Извлечение по `seq` приходящего ack.
    pub fn resolve(&mut self, seq: u32) -> Option<PendingCommand> {
        self.map.remove(&seq)
    }

    /// Скан таймаутов: идемпотентные (≤1 повтора) → retry, остальные → expired.
    pub fn time_outs(&mut self, timeout: Duration, now: Instant) -> Timeouts {
        let mut out = Timeouts::default();
        let expired_seqs: Vec<u32> = self
            .map
            .iter()
            .filter(|(_, p)| now.duration_since(p.sent_at) >= timeout)
            .map(|(seq, _)| *seq)
            .collect();

        for seq in expired_seqs {
            let Some(pending) = self.map.get_mut(&seq) else {
                continue;
            };
            if pending.idempotent && pending.attempts < 2 {
                pending.attempts += 1;
                pending.sent_at = now;
                out.retry.push(pending.envelope.clone());
            } else {
                let pending = self.map.remove(&seq).expect("ключ из итерации");
                out.expired.push((seq, pending.envelope.msg_type));
            }
        }
        out
    }

    /// Откат всех pending при обрыве соединения (§2.4: без ретраев через
    /// реконнект — неидемпотентные теряются явно, приложение решает само).
    pub fn drain(&mut self) -> Vec<PendingCommand> {
        self.map.drain().map(|(_, p)| p).collect()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ld2_protocol::cmd::{LocationMove, Ping};

    fn cmd_seq<C: ld2_protocol::Command>(seq: u32, c: &C) -> Envelope {
        Envelope::command(seq, "2026-11-03T00:00:00Z", c).expect("envelope")
    }

    #[test]
    fn resolve_takes_pending() {
        let mut q = PendingQueue::new();
        q.insert(cmd_seq(1, &Ping {}), true);
        assert_eq!(q.len(), 1);
        let p = q.resolve(1).expect("pending есть");
        assert_eq!(p.envelope.seq, Some(1));
        assert!(q.resolve(1).is_none(), "повторный resolve пуст");
        assert!(q.is_empty());
    }

    #[test]
    fn idempotent_retries_once_then_expires() {
        let mut q = PendingQueue::new();
        q.insert(cmd_seq(7, &Ping {}), true);
        let t0 = Instant::now();

        // До таймаута — тишина
        assert!(q
            .time_outs(Duration::from_secs(10), t0 + Duration::from_secs(5))
            .retry
            .is_empty());

        // Первый таймаут → один повтор тем же seq
        let out = q.time_outs(Duration::from_secs(10), t0 + Duration::from_secs(11));
        assert_eq!(out.retry.len(), 1);
        assert_eq!(
            out.retry[0].seq,
            Some(7),
            "повтор с тем же seq (окно сервера)"
        );
        assert!(out.expired.is_empty());

        // Второй таймаут → expired (только 1 повтор, §2.4)
        let out = q.time_outs(Duration::from_secs(10), t0 + Duration::from_secs(22));
        assert!(out.retry.is_empty());
        assert_eq!(out.expired, vec![(7, "cmd/ping".to_string())]);
        assert!(q.is_empty());
    }

    #[test]
    fn non_idempotent_never_retries() {
        let mut q = PendingQueue::new();
        q.insert(
            cmd_seq(
                9,
                &LocationMove {
                    exit_id: "e_n".into(),
                },
            ),
            false,
        );
        let t0 = Instant::now();
        let out = q.time_outs(Duration::from_secs(10), t0 + Duration::from_secs(11));
        assert!(out.retry.is_empty(), "неидемпотентная команда не ретраится");
        assert_eq!(out.expired.len(), 1);
    }

    #[test]
    fn drain_reports_all_on_disconnect() {
        let mut q = PendingQueue::new();
        q.insert(cmd_seq(1, &Ping {}), true);
        q.insert(cmd_seq(2, &Ping {}), true);
        let drained = q.drain();
        assert_eq!(drained.len(), 2);
        assert!(q.is_empty());
    }

    #[test]
    fn window_capped_at_128() {
        let mut q = PendingQueue::new();
        for seq in 0..200 {
            q.insert(cmd_seq(seq, &Ping {}), true);
        }
        assert!(q.len() <= MAX_PENDING, "окно ≤ 128 (§2.4)");
    }
}
