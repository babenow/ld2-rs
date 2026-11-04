//! OS-уведомления (D7, НФР обновления/события): 4+ события — смерть,
//! приглашения (пати/гильдия), открытие сделки, whisper, результат
//! улучшения. Флаг — `config.notify` (feature-flag `desktop.notify`).
//!
//! Показ — в отдельном потоке: dbus/NSUserNotification может блокировать,
//! а НФР §8 требует UI-фризы ≤ 16 мс.

/// Показ системного уведомления (ошибка — в лог, не фатально).
pub fn show(body: &str) {
    let body = body.to_string();
    std::thread::spawn(move || {
        let result = notify_rust::Notification::new()
            .summary("ld2")
            .body(&body)
            .show();
        if let Err(e) = result {
            tracing::warn!("OS-уведомление не показано: {e}");
        }
    });
}
