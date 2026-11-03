# План M1 — ld2-rs «Нить» (контракт + транспорт + мок)

**Статус: M1 «Нить» ЗАВЕРШЁН (2026-11-03).** T1 ✓ (PR #1) · T2 ✓ (PR #3) · T3 ✓ (PR #4) · T4 ✓ (PR #5, squash `51f3c8e`) — все влиты в `develop`, CI green 3 ОС, **49 тестов**, DoD §10 закрыт обеими частями (снапшот+ack, реконнект ≤ 30 с), тег **`m1-done`** поставлен.
**Ветка интеграции:** `develop` (git flow). *Замечание: PR #2 (develop→main) смержен мимо процедуры release/* — согласовать с владельцем. Урок T4: удалять ветку ТОЛЬКО после проверки `mergedAt` (досрочное удаление закрыло PR #5 — восстановлен по SHA).*
**Следующий этап — M2 (аккаунт/персонажи): экраны входа и выбора, мастер «имя + профиль» (D3), keyring-refresh → транспорт.**
**Источники норм:** `docs/protocol.md` (канон контракта), `docs/tz.md` §4/§10 (M1), `ROADMAP.md` (v0.1), `CONTRIBUTING.md`.

**DoD M1 (ТЗ §10, дословно):** «demo-аккаунт → снапшот → команды с ack; обрыв сети → восстановление ≤ 30 с без ручных действий (тест)» + CI green (fmt/clippy -D warnings/test/build, 3 ОС).

---

## 1. Задачи и ветки

| # | Ветка | Содержание | Критерий готовности |
|---|---|---|---|
| **T1 ✓** (PR #1) | `feature/M1-protocol` *(удалена)* | **Полная типизация контракта v1** по `protocol.md`: `types.rs` (§6), `cmd.rs` (§7, все 58 команд + `IDEMPOTENT`), `evt.rs` (§8, 37 событий + `PatchOp`), `errors.rs` (§9, 35 кодов + `Unknown`), `features.rs` (§5 `SessionConfig` c capability-flatten), хелперы `Envelope` (command/payload_as), `Ack`, golden round-trip тесты на каждый домен | ✅ `cargo test -p ld2-protocol` зелёный (21 тест); wire-`v` = 1; сверка таблиц §7/§8/§9 golden-тестами |
| **T2 ✓** (PR #3) | `feature/M1-transport` *(удалена)* | ws-транспорт клиента: tokio-рантайм в рабочем потоке, `seq`/ack-матчер (окно 128, кэш ack), таймаут 10 с + идемпотентный повтор ×1, backoff reconnect 0.5→30 с ±20% jitter, `auth.resume` → `state.resync`, статус-машина `online/reconnecting/offline/auth_expired`, mpsc-каналы Ingress/Outbound ↔ AppStore | ✅ интеграционный тест: подключение, команда→ack, обрыв→восстановление ≤ 30 с (resume+resync, `state_version` отслеживается), политика ретраев; 33 теста, CI green |
| **T3 ✓** (PR #4) | `feature/M1-mock` *(удалена)* | Мок-сервер: держит контракт (cmd→ack c `state_version`), сценарии `hello` (auth.login demo/demo123 → session.start), `snapshot` (char.select → полный снапшот), `chaos` (случайные обрывы WS с seed), `cmd/ping`→`evt/pong`, `time.sync` | ✅ 12 контрактных тестов + chaos unit; CLI `--scenario/--seed/--chaos-every`; смоук `--help`/запуск; фикстуры из `docs/skills.md` §9 |
| **T4 ✓** (PR #5, `51f3c8e`) | `feature/M1-e2e` *(удалена)* | Сквозные тесты transport↔mock: ack-timeout, идемпотентный повтор, resync после обрыва, порядок событий; smoke `demo/demo123` → снапшот → `location.move`→ack | ✅ DoD M1: вход→снапшот→ack + обрыв→авто-reconnect ≤ 30 с с монотонным `state_version`; 49 тестов, CI green |

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
