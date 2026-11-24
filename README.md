# ld2-rs

**ld2** — desktop-клиент текстовой онлайн-MMORPG, написанный на Rust с нуля.

Мир игры — виртуальная карта текстовых локаций с переходами («тропа на север», «дорога на
запад»). Персонаж прокачивает навыки через опыт (квесты, бои), теряет весь лут при смерти,
занимается профессиями и крафтом, кастует магию со свитков и рун, инкрустированных в посохи,
улучшает предметы и живёт в социальном слое: чат, группы, торговля, гильдии — и система
преступлений со статусами («Убийца», «Мародёр») и таймерами наказания.

> **Статус проекта:** M0–M23 реализованы (v0.22.0, смоук заказчиком по команде), M24 в работе;
> клиент, сервер и редактор собраны, CI — матрица 3 ОС. См. [`ROADMAP.md`](./ROADMAP.md),
> [`docs/tz.md`](./docs/tz.md) и [`CONTRIBUTING.md`](./CONTRIBUTING.md).

---

## Документация

| Документ | Содержание |
|---|---|
| [`docs/tz.md`](./docs/tz.md) | Техническое задание + промпт: функциональные требования (ФР-01…19), контракт протокола (JSON over WebSocket), архитектура, UX, milestone-план M0–M9, NFR, тестирование, релизы |
| [`docs/protocol.md`](./docs/protocol.md) | **Полный контракт «клиент ↔ сервер»**: транспорт, конверт, snapshot/delta/resync, команды и события с payload-схемами, коды ошибок, модель преступности, примеры диалогов, чек-листы авторов сервера и клиента |
| [`docs/skills.md`](./docs/skills.md) | GDD: атрибуты, модель опыта (skill XP + XP-пул), 34 навыка в 6 группах, активные способности, 14 профессий, цепочки сырья, баланс-константы |
| [`CONTRIBUTING.md`](./CONTRIBUTING.md) | Правила работы: git, Git Flow (`main`/`develop`/`feature`/`release`/`hotfix`), Conventional Commits, SemVer + теги `vX.Y.Z` / `m<n>-done`, PR и ревью |
| `docs/decisions/` | ADR-журнал: 001 — GPG-подпись как рекомендация, 002 — мана + заряды рун, 003 — контент-решения v1 (профили, статусы, слоты, уведомления, локализация, карта) |
| [`PLAN-M0.md`](./PLAN-M0.md) | План реализации M0: журнал решений D1–D10, фазы 0–2 с DoD, ограничения и риски |

## Технологии

- **Язык:** Rust (стабильный toolchain, зафиксирован в `rust-toolchain.toml`)
- **UI:** [gpui](https://gpui.rs/) (Zed) + [Ely](https://elygpui.com/) — единственный UI-слой (ADR-004)
- **Асинхронность и сеть:** `tokio` + WebSocket (push-события в реальном времени)
- **Сервер:** `ld2-server` — PostgreSQL (sqlx, авто-миграции), настройки в `server.toml`
- **Тесты:** golden-тесты контракта, контрактные/e2e-тесты против реального сервера с БД
- **CI/CD:** GitHub Actions, матрица Windows / macOS / Linux, релизные артефакты по тегу

## Структура репозитория (по ТЗ §9)

```
ld2-rs/
├── crates/
│   ├── ld2-protocol/       # serde-типы контракта §4 ТЗ (без UI/сети)
│   ├── ld2-content/        # схема, загрузка и валидация контента (сервер + редактор)
│   ├── ld2-core/           # ядро клиента: store/transport/l10n/config
│   ├── ld2-client/         # desktop-клиент (gpui + Ely, бинарь ld2-gpui)
│   ├── ld2-server/         # сервер: PostgreSQL, server.toml, WS §1/§2 (бинарь ld2-server)
│   ├── ld2-editor/         # редактор контента (gpui + Ely, бинарь ld2-editor)
│   └── ld2-test-kit/       # тестовая поддержка: WS-харнесс, проверки, фикстуры (dev-only)
├── content/                # рантайм-контент: 8 JSON (locations, skills, items, …)
├── deploy/                 # server.toml для контейнера
├── docs/                   # tz.md · protocol.md · skills.md · decisions/ · sources/
├── scripts/                # аудит рефакторинга, иконки, бэкап
├── .github/workflows/      # ci.yml, release.yml
└── Dockerfile · docker-compose.yml · README · ROADMAP · HISTORY · CONTRIBUTING
```

## Сборка и запуск

```bash
# клиент (gpui; бинарь ld2-gpui)
cargo run -p ld2-client

# реальный сервер: PostgreSQL + сервер одной командой (docker compose)
docker compose up --build -d
curl http://127.0.0.1:24817/healthz        # → 200 ok

# локальный запуск сервера без Docker (конфиг: шаблон server.example.toml)
cp crates/ld2-server/server.example.toml server.toml
cargo run -p ld2-server -- --config server.toml check   # валидация конфига

# тесты + статический анализ (тесты с БД — при заданном LD2_TEST_DATABASE_URL)
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Хост по умолчанию — `ws://127.0.0.1:24817/ws` (настраивается в конфиге клиента;
релизная сборка работает только по `wss://`). Все настройки сервера — в `server.toml`
(шаблон с комментариями — `crates/ld2-server/server.example.toml`).

## Дорожная карта

Полная карта по версиям — в [`ROADMAP.md`](./ROADMAP.md); детали и критерии приёмки — таблица §10 ТЗ. Кратко:

| Версии | Вехи | Результат |
|---|---|---|
| v0.1–v0.9 | M1–M9 | контракт v1 и WS-транспорт, вход и персонажи, мир/бой/экономика, квесты и профессии, чат/группы/торговля/гильдии, полировка и релизные артефакты 3 ОС |
| v0.10–v0.12 | M10–M12 | второй клиент на gpui (Zed) и Ely, реальный сервер на PostgreSQL |
| v0.13–v0.19 | M13–M19 | регистрация с email, окна персонажа/рюкзака, боевые системы и полировка UI |
| v0.20–v0.22 | M20–M23 | мир (респавны, лут, ресурсы, сундуки), профессии, снятие шкур, окно подятия |
| v0.23–v0.24 | M24, M26 | админ-панель предметов (в работе), карта-граф мира (план) |

Подробно по каждой версии — в [`ROADMAP.md`](./ROADMAP.md); журнал работ — [`HISTORY.md`](./HISTORY.md).

## Участие в разработке

Правила веток, коммитов и версионирования — в [`CONTRIBUTING.md`](./CONTRIBUTING.md)
(Git Flow + Conventional Commits + SemVer в lockstep по crate-ам workspace).
Любая работа берётся из milestone-плана ТЗ; изменения контракта протокола — только
через PR `[tz-amend]`.

## Лицензия

TBD (будет указана при публичном релизе кода).
