//! Управляемые «ручки» мока для сквозных тестов (PLAN-M1 T4): запись
//! полученных конвертов, управляемые обрывы, события при подключении.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ld2_protocol::Envelope;

#[derive(Default)]
pub struct Knobs {
    /// Принятых соединений (нумерация с 1).
    pub connections: AtomicUsize,
    /// Все полученные конверты (сквозь соединения, в порядке прихода).
    pub received: Mutex<Vec<Envelope>>,
    /// Отвечать ли ack'ами (false = молчим → сценарий таймаута).
    pub respond: AtomicBool,
    /// Закрыть соединение №1 после N ack'ов (0 = никогда) → сценарий обрыва.
    pub drop_first_conn_after: AtomicUsize,
    /// Прислать `evt/pong` сразу после подключения.
    pub send_pong_on_connect: AtomicBool,
}

impl Knobs {
    /// Как в `tests/common` старого стенда: respond=true по умолчанию.
    pub fn active() -> Arc<Self> {
        let knobs = Arc::new(Self::default());
        knobs.respond.store(true, Ordering::SeqCst);
        knobs
    }

    pub fn received_count(&self, msg_type: &str) -> usize {
        self.received
            .lock()
            .expect("mutex")
            .iter()
            .filter(|e| e.msg_type == msg_type)
            .count()
    }

    pub fn find(&self, msg_type: &str) -> Option<Envelope> {
        self.received
            .lock()
            .expect("mutex")
            .iter()
            .find(|e| e.msg_type == msg_type)
            .cloned()
    }
}
