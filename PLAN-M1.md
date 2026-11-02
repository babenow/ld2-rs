# План M1 — ld2-rs «Нить» (контракт + транспорт + мок)

**Статус:** в работе (с 2026-11-03). **Ветка интеграции:** `develop` (git flow).
**Источники норм:** `docs/protocol.md` (канон контракта), `docs/tz.md` §4/§10 (M1), `ROADMAP.md` (v0.1), `CONTRIBUTING.md`.

**DoD M1 (ТЗ §10, дословно):** «demo-аккаунт → снапшот → команды с ack; обрыв сети → восстановление ≤ 30 с без ручных действий (тест)» + CI green (fmt/clippy -D warnings/test/build, 3 ОС).

---

## 1. Задачи и ветки

| # | Ветка | Содержание | Критерий готовности |
|---|---|---|---|
| T1 | `feature/M1-protocol` | **Полная типизация контракта v1** по `protocol.md`: `types.rs` (§6), `cmd.rs` (§7, все ~50 команд), `evt.rs` (§8, все события), `errors.rs` (§9, полный каталог `E_*`), `features.rs` (§5 `SessionConfig` c capability-flatten), хелперы `Envelope` (command/payload_as), golden round-trip тесты на каждый домен | `cargo test -p ld2-protocol` зелёный; wire-`v` заморожен = 1; ни одно поле §4 не потеряно (сверка по таблицам §7/§8) |
| T2 | `feature/M1-transport` | ws-транспорт клиента: tokio-рантайм в рабочем потоке, `seq`/ack-матчер (окно 128, кэш ack), таймаут 10 с + идемпотентный повтор ×1, backoff reconnect 0.5→30 с ±20% jitter, `auth.resume` → `state.resync`, статус-машина `online/reconnecting/offline/auth_expired`, mpsc-каналы Ingress/Outbound ↔ AppStore | интеграционный тест: подключение к моку, команда→ack, обрыв→восстановление ≤ 30 с |
| T3 | `feature/M1-mock` | Мок-сервер: держит контракт (cmd→ack c `state_version`), сценарии `hello` (auth.login demo/demo123 → session.start), `snapshot` (char.select → полный снапшот), `chaos` (случайные обрывы WS с seed), `cmd/ping`→`evt/pong`, `time.sync` | ручной прогон + автотесты сценариев; фикстуры навыков/профессий — из `docs/skills.md` §9 |
| T4 | `feature/M1-e2e` | Сквозные тесты transport↔mock: ack-timeout, идемпотентный повтор, resync после обрыва, порядок событий; smoke `demo/demo123` → снапшот → `location.move`→ack | все тесты в CI; DoD-пункт «восстановление ≤ 30 с» закрыт автоматически |

Каждая задача → PR в `develop` (squash), после T4 — тег `m1-done` на `develop` (CONTRIBUTING §5.3) и `v0.1.0`-rc по мере стабилизации.

## 2. Правила на M1 (не нарушать)

1. **Типы только в `ld2-protocol`** — транспорт/мок/клиент импортируют, не дублируют (ТЗ §15.3).
2. **Изменение контракта = правка `docs/protocol.md` → типы → golden-тесты**, в этом порядке; несовместимое — bump `v` (§11 protocol).
3. **Capability-first:** `SessionConfig` с `#[serde(flatten)]` для незнакомых ключей; хардкодов лимитов в коде нет (ТЗ §15.4).
4. Неизвестные `type`/поля — игнор с `tracing::warn`, никогда не паниковать (§2.3).
5. Время: `ts` = ISO-8601, дедлайны = epoch-секунды; клиент калибрует `time_offset` из `evt/time.sync` (§1.1).
6. Коммиты: conventional, subject по-русски, футер `Milestone: M1`; PR описывает связь с ФР/§.

## 3. Порядок исполнения

```
T1 protocol ──► T2 transport ──► T3 mock ──► T4 e2e ──► m1-done
       (типы)        (живой клиент)   (живой стенд)   (DoD-тесты)
```

T2 и T3 частично параллельны (общий контракт из T1), T4 замыкает.

## 4. Риски

| Риск | Мера |
|---|---|
| Объём типов (~90 сообщений) | по доменам + golden-тест на каждый; сверка с таблицами §7/§8 чек-листом |
| Расхождение мока и документа | мок пишется **по** `protocol.md`/типам, не по памяти; отклонение → правка документа `[tz-amend]` |
| Реконнект-гонки | `state_version` + правило «старый ack/событие игнор» (§2.4); тест T4 на chaos-сценарии |
| Долгие CI-прогоны | кэш `Swatinem/rust-cache` уже включён |

## 5. Открытые вопросы M1

нет (контракт заморожен в `docs/protocol.md` v1; изменения — только через `[tz-amend]`).
