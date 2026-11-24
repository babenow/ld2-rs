# ld2-rs

**ld2** — desktop-клиент текстовой онлайн-MMORPG, написанный на Rust с нуля.

Мир игры — виртуальная карта текстовых локаций с переходами («тропа на север», «дорога на
запад»). Персонаж прокачивает навыки через опыт (квесты, бои), теряет весь лут при смерти,
занимается профессиями и крафтом, кастует магию со свитков и рун, инкрустированных в оружие,
улучшает предметы и живёт в социальном слое: чат, группы, торговля, гильдии — и система
преступлений со статусами («Убийца», «Мародёр») и таймерами наказания. Инвентарь —
контейнерный: базовые ячейки и сумки (M28).

> **Статус проекта:** M0–M27 реализованы — клиент (gpui+Ely), реальный сервер на PostgreSQL,
> редактор контента `ld2-editor` и инфраструктура (CI 3 ОС, Docker, релизные артефакты).
> Релиз — `v0.22.0` (M23); изменения M24–M27 и система сумок M28 входят в незарелизенную `v0.23.0+`
> (вехи отмечены тегами `m26-done`/`m27-done`). Рефакторинг RF0–RF5 закрыт: 313 src-тестов
> и e2e-набор. См. [`ROADMAP.md`](./ROADMAP.md), [`HISTORY.md`](./HISTORY.md) и
> [`CONTRIBUTING.md`](./CONTRIBUTING.md).

---

## Документация

| Документ | Содержание |
|---|---|
| [`ROADMAP.md`](./ROADMAP.md) | Дорожная карта: M0–M27 по версиям, статусы вех, backlog после v1.0 |
| [`docs/tz.md`](./docs/tz.md) | Техническое задание: функциональные требования (ФР-01…19), контракт протокола (JSON over WebSocket), архитектура, UX, исходный milestone-план M0–M9, NFR |
| [`docs/protocol.md`](./docs/protocol.md) | **Полный контракт «клиент ↔ сервер»**: транспорт, конверт, snapshot/delta/resync, команды и события с payload-схемами, коды ошибок, модель преступности, discovery `well-known`, примеры диалогов, чек-листы авторов сервера и клиента |
| [`docs/data-model.md`](./docs/data-model.md) | Модель данных: пять слоёв (контент, PostgreSQL, память сервера, стор клиента, UI), миграции, таблицы, инварианты |
| [`docs/skills.md`](./docs/skills.md) | GDD: атрибуты, модель опыта (skill XP + XP-пул), 34 навыка в 6 группах, активные способности, 14 профессий, цепочки сырья, баланс-константы |
| [`docs/professions.md`](./docs/professions.md) | Справочник профессий: уровни/ранги, цены, источники опыта, снятие шкур, контент и wire-команды |
| [`docs/deployment.md`](./docs/deployment.md) | Эксплуатация сервера: установка, reverse-proxy с `wss`, миграции, бэкап/восстановление, конфиг-reference `server.toml` |
| [`docs/user-manual.md`](./docs/user-manual.md) | Руководство игрока: установка, интерфейс, окна, чат, настройки, диагностика |
| [`crates/ld2-editor/README.md`](./crates/ld2-editor/README.md) | Редактор контента: назначение, установка, поток работы контент-мейкера |
| [`CONTRIBUTING.md`](./CONTRIBUTING.md) | Правила работы: git, Git Flow, Conventional Commits, SemVer lockstep, теги `vX.Y.Z` / `m<n>-done`, PR и ревью, бюджет рефакторинга |
| [`HISTORY.md`](./HISTORY.md) | Дневник работ: записи по датам, решения, проверки |
| [`CHANGELOG.md`](./CHANGELOG.md) | Агрегация изменений по вехам для релизных заметок |
| `docs/decisions/` | ADR-журнал (001–013): GPG-подпись, мана и заряды рун, контент v1, Ely/gpui, удаление персонажа, auth/регистрация, диалоги, тренер, бонусы предметов, камни заточки, руна-сокеты, discovery |

## Технологии

- **Язык:** Rust, toolchain зафиксирован в `rust-toolchain.toml` (1.100.0)
- **UI:** [gpui](https://gpui.rs/) (Zed) + [Ely](https://elygpui.com/) — единственный UI-слой (ADR-004); редактор — тот же стек
- **Асинхронность и сеть:** `tokio` + WebSocket (push-события в реальном времени)
- **Сервер:** `ld2-server` — PostgreSQL (sqlx, авто-миграции 0001–0028), настройки в `server.toml`, hot reload контента (poll/SIGHUP)
- **Контент:** `ld2-content` — общая схема/валидация 8 JSON для сервера и редактора
- **Тесты:** golden-тесты контракта, unit/reducer-тесты, контрактные/e2e-тесты против реального сервера с БД (тестовая БД `ld2_test`)
- **CI/CD:** GitHub Actions, матрица Windows / macOS / Linux, релизные артефакты по тегу + SHA256SUMS

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
├── docs/                   # tz.md · protocol.md · skills.md · data-model.md · deployment.md · user-manual.md · decisions/ · sources/
├── scripts/                # аудит рефакторинга, иконки, бэкап
├── .github/workflows/      # ci.yml, release.yml
└── Dockerfile · docker-compose.yml · README · ROADMAP · HISTORY · CHANGELOG · CONTRIBUTING
```

## Сборка и запуск

```bash
# клиент (gpui; бинарь ld2-gpui)
cargo run -p ld2-client

# редактор контента (gpui; бинарь ld2-editor)
cargo run -p ld2-editor

# реальный сервер: PostgreSQL + сервер одной командой (docker compose)
docker compose up --build -d
curl http://127.0.0.1:24817/healthz        # → 200 ok

# локальный запуск сервера без Docker (конфиг: шаблон server.example.toml)
cp crates/ld2-server/server.example.toml server.toml
cargo run -p ld2-server -- --config server.toml check   # валидация конфига

# тесты + статический анализ (тесты с БД — при заданном LD2_TEST_DATABASE_URL)
LD2_TEST_DATABASE_URL=postgres://ld2:ld2@127.0.0.1:5432/ld2_test cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check

# аудит рефакторинга (прод-строки, функции >200, бюджет)
scripts/refactor-audit.sh report
scripts/refactor-audit.sh --verify
```

Хост по умолчанию — `ws://127.0.0.1:24817/ws` (настраивается в конфиге клиента;
в release-сборке — `wss://`, кроме localhost). Все настройки сервера — в `server.toml`
(шаблон с комментариями — `crates/ld2-server/server.example.toml`).

## Дорожная карта

Полная карта по версиям — в [`ROADMAP.md`](./ROADMAP.md); детали и критерии приёмки — таблица §10 ТЗ. Кратко:

| Версии | Вехи | Результат |
|---|---|---|
| v0.1–v0.9 | M1–M9 | контракт v1 и WS-транспорт, вход и персонажи, мир/бой/экономика, квесты и профессии, чат/группы/торговля/гильдии, полировка и релизные артефакты 3 ОС |
| v0.10–v0.12 | M10–M12 | второй клиент на gpui (Zed) и Ely, реальный сервер на PostgreSQL |
| v0.13–v0.19 | M13–M19 | регистрация с email, окна персонажа/рюкзака, боевые системы и полировка UI, discovery подключения |
| v0.20–v0.23 | M20–M26 | мир (респавны, лут, ресурсы, сундуки), профессии, снятие шкур, окно подятия, админ-панель, ад-мир и босс-дракон, карта-граф мира |
| v0.24+ | M27 | hot reload контента и редактор `ld2-editor` (E0–E6.2) |

Подробно по каждой версии — в [`ROADMAP.md`](./ROADMAP.md); журнал работ — [`HISTORY.md`](./HISTORY.md).

## Участие в разработке

Правила веток, коммитов и версионирования — в [`CONTRIBUTING.md`](./CONTRIBUTING.md)
(Git Flow + Conventional Commits + SemVer в lockstep по crate-ам workspace).
Любая работа берётся из milestone-плана (ROADMAP/plan текущей вехи); изменения контракта
протокола — только через PR `[tz-amend]` + ADR. Бюджет рефакторинга — `scripts/refactor-budget.txt`
и `scripts/refactor-audit.sh` (может только уменьшаться).

## Лицензия

TBD (будет указана при публичном релизе кода).
