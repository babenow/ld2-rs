# План M0 — ld2-rs (каркас и инфраструктура)

**Статус:** Фазы 0–2 **выполнены локально** (2026-11-03): документы + ADR-001…003, `git init` + первый коммит, workspace из 3 crate'ов, CI-воркфлоу и хуки созданы.
**Осталось (F1.4–F1.5):** `gh auth refresh -h github.com` → `gh repo create` → push → зелёный прогон Actions на 3 ОС.
**Окружение (проверено):** macOS, git 2.56, cargo/rustc 1.99.1 stable, rustfmt 1.10, clippy, gh 2.103.
**Проблема:** `gh auth` для аккаунта `babenow` — токен в keyring недействителен → пользователь обязан выполнить `gh auth refresh -h github.com` до создания репозитория (шаг F1.4).

**Источники норм:** `docs/tz.md` (§9, §10 M0, §12, §15), `docs/protocol.md`, `docs/skills.md`, `CONTRIBUTING.md`, `ROADMAP.md`.

---

## 1. Журнал решений (обязателен к учёту; правки документов — Фаза 0)

| # | Решение | Детали |
|---|---|---|
| D1 | **Хостинг:** GitHub с первого дня | приватный репо `ld2-rs` (по умолчанию; поменять на public — просьба пользователя), Actions CI + Releases |
| D2 | **GPG-подпись коммитов → рекомендация** | `git config commit.gpgsign true` НЕ ставим; правка CONTRIBUTING §2 + ADR-001 |
| D3 | **Создание персонажа:** имя + стартовый профиль | профили: Воин/Вор/Маг (навыки своей группы 0, остальные 0.25); без рас/классов в v1. `cmd/char.create { name, profile }`, список профилей — capability-поле от сервера |
| D4 | **Мана — есть. Двойной ресурс: мана + заряды рун** | `mp: {cur, max}` вместо `mp: null`; max = `20 + int×4 + arcana×5` (конфиг). Свиток = одноразка, ману не тратит. Руна в посохе: каст тратит **ману И заряд**; заряды восстанавливаются маслом алхимика. Воинские активки — только кулдаун. Реген маны: вне боя быстро, в бою медленно (конфиг сервера) |
| D5 | **Статус «Вор» (thief) — включить в моке** | 3 статуса: Убийца, Мародёр, Вор (карманная кража у игроков); `kinds_thief.enabled = true` в конфиге мока; `pickpocket` против игроков разрешён |
| D6 | **Слоты персонажей мока = 8** | `char_slot_limit: 8` (динамическое поле, не хардкод) |
| D7 | **OS-уведомления (по умолчанию, feature-flag):** | смерть персонажа · приглашение в пати/сделку · личные сообщения · результат улучшения предмета |
| D8 | **en-локализация: P1** | с M0 поддержка таблиц `ru/en` в `l10n`, полный перевод — M9 |
| D9 | **Карта-граф: не в v1** | v1 = список посещённых локаций + быстрые переходы; граф — v1.1 (уже в backlog ROADMAP) |
| D10 | **Дефолты без обсуждения** | константы преступности `k=0.5, T_MAX=604800, murderer=21600, marauder=7200`; чужие бои — только текст локации; гильдейский банк вне v1; состав 28 навыков / 8 профессий; `profession_limit=2`; `dark` — вне M7; деградации навыков нет |

Все решения фиксируются ADR-файлами (см. Фаза 0) — это и есть ответы на §14 ТЗ и §10 skills.

---

## 2. Фаза 0 — правки документов под решения (до первого коммита кода)

> Файлы: `docs/tz.md`, `docs/protocol.md`, `docs/skills.md`, `CONTRIBUTING.md`.

**2.1 `docs/skills.md`:**
- §1: убрать «маны нет», ввести `mp` (макс = 20 + int×4 + arcana×5);
- §2: раздел «два потока опыта» — дополнить: касты рун тратят ману; свитки — без маны;
- §4 активки: явно «только кулдаун, ману не тратят»;
- §5.2 `alchemist`: масло = восстановление зарядов рун (оставить) + добавить «зелья маны» в продукцию;
- §8 конфиг: `kinds_thief.enabled = true`, добавить блок `[mana]`;
- §10 закрыть пункты: 2 (лимит=8 → D6), 3 (мана → D4), 4 («Вор» → D5), 7 (деградации нет — подтверждено).

**2.2 `docs/protocol.md`:**
- §6 типы: `Mp { cur, max }`; в снапшоте `char.mp` — объект (не null); поля `mp_left` в `evt/magic/cast`;
- §5 `session.start`: в `crime.kinds` добавить `{"kind":"thief","title":"Вор","severity":"medium"}`;
- §7: `cmd/char.create` payload → `{ name, profile }`; новая команда-не нужна;
- §9: новый код `E_NOT_ENOUGH_MP` (+ `E_BAD_PROFILE` для невалидного профиля);
- §12: обновить примеры (mp в снапшоте, стоимость каста);
- §13/§14 чек-листы: упомянуть mp-проверку.

**2.3 `docs/tz.md`:**
- ФР-02: создание = имя + профиль (D3);
- ФР-05/ФР-09: mp-бар в бою и листе; руна = мана+заряд; свиток без маны (D4);
- ФР-11: упомянуть статус «Вор» (D5);
- ФР-18: список уведомлений по умолчанию = 4 события (D7);
- §14: закрыть пункты 1 (константы → D10), 2 (→ D3), 4 (→ D9), 5 (→ D7), 6 (→ D8), 8 (→ D6), 3 (PvP-трансляция текстом → D10), 7 (банк → D10).

**2.4 `CONTRIBUTING.md`:** §2 — `git config commit.gpgsign true` убрать, комментарий «подписываемые коммиты (обязательно)» → «рекомендуются, см. ADR-001»; §7 — `core.hooksPath` остаётся рекомендацией.

**2.5 ADR (`docs/decisions/`):**
- `001-gpg-signing-optional.md` — D2;
- `002-mana-and-rune-charges.md` — D4 (модель двойного ресурса, влияние на баланс);
- `003-v1-content-decisions.md` — D3, D5, D6, D7, D8, D9 (сводка дефолтов, ссылки на §14/§10).

Формат ADR — 1 страница: Контекст / Решение / Последствия (CONTRIBUTING §15.7 ТЗ).

**DoD Фазы 0:** все ссылки в md валидны (автопроверка), мусорных символов нет, решения не противоречат `protocol.md`.

---

## 3. Фаза 1 — git, GitHub, CI

1. `git init -b main` в `/Volumes/Babenow/Projects/ld2-rs`.
2. `.gitignore` по CONTRIBUTING §2 (`target/`, IDE, `.DS_Store`, `*.local.toml`, `client-data/`); `Cargo.lock` — коммитить (появится в Фазе 2).
3. `rust-toolchain.toml`: `stable` + компоненты `rustfmt, clippy`.
4. **Пользователь:** `gh auth refresh -h github.com` (токен недействителен — интерактивно, агент не может).
5. `gh repo create ld2-rs --private --source . --push` (public — по подтверждению).
6. Первый коммит: все 6 документов + `.gitignore` + `rust-toolchain.toml` (+ ADR из Фазы 0).
   Формат по CONTRIBUTING §4: `chore(build): инициализация репозитория с документацией` (type/scope — EN, subject — RU), футеры `Milestone: M0`.
   GPG не требуется (D2/ADR-001).
7. `.github/workflows/ci.yml`: матрица `[ubuntu-22.04, macos-14, windows-latest]`; шаги: checkout → toolchain → `cargo fmt --check` → `cargo clippy --workspace --all-targets -- -D warnings` → `cargo test --workspace` → `cargo build --workspace`. Порог: зелёный прогон.
8. `.githooks/pre-commit` (fmt+clippy) + инструкция в README/CONTRIBUTING (рекомендация: `git config core.hooksPath .githooks`).

**DoD Фазы 1:** remote запушен, Actions CI зелёный на 3 ОС, первый коммит в `main`.

---

## 4. Фаза 2 — workspace и пустое окно (ядро M0)

Структура строго по `docs/tz.md` §9:

```
ld2-rs/
├── Cargo.toml                 # [workspace] members = crates/*, resolver = "2"
├── rust-toolchain.toml
├── crates/
│   ├── ld2-protocol/          # lib: заглушки модулей envelope/cmd/evt/errors/features (типы M1 — не проектировать сейчас)
│   ├── ld2-mock-server/       # bin: tokio + заглушка (порт 24817, лог «мок поднят»)
│   └── ld2-client/            # bin: eframe-окно «ld2 — M0»
│       └── src/{main.rs, app.rs, config.rs, l10n/{mod.rs, ru.rs, en.rs}, transport/, store.rs}
├── .github/workflows/ci.yml
├── .githooks/pre-commit
└── docs/ · README.md · CONTRIBUTING.md · ROADMAP.md
```

Задачи (порядок):
1. `Cargo.toml` workspace + `cargo new` три crate'а; версии всех crate'ов = `0.1.0` (lockstep, CONTRIBUTING §5.1 — bump на релиз).
2. `ld2-client`: минимальный `eframe` (последний stable egui): окно 960×640, заголовок «ld2», `l10n` с двумя таблицами (D8: поддержка ru/en с порога), кнопка «Выход». Кириллица: embedded-шрифт (PT Sans/DejaVu) в `assets/fonts/` — не полагаться на системный.
3. `config.rs`: загрузка/создание `config.toml` в каталоге `directories` (host для dev = `ws://127.0.0.1:24817/ws`); лог `tracing` + rolling-файлы в каталоге данных.
4. `secrets.rs`: smoke-тест keyring (записать/прочитать/удалить токен `test`) — требование DoD M0.
5. `ld2-mock-server`: поднять WS-слушатель на 24817, отвечать на любое сообщение эхом — для ручной проверки (реальный контракт — M1).
6. `ld2-protocol`: `Envelope`-заглушка с полем `v` + тест round-trip — остальное M1.
7. Локальные хуки: `git config core.hooksPath .githooks`.

**DoD M0 (§10 ТЗ, дословно + расшифровка):**
- [x] `cargo build --workspace` и `cargo run -p ld2-client` — окно стартует на macOS (Linux/Windows — проверяются CI-прогоном после push);
- [ ] CI зелёный на 3 ОС (fmt, clippy -D warnings, test, build) — *ждёт `gh auth refresh` + push*;
- [x] лог пишется в файл (rolling `ld2.log.<дата>` в `<data>/logs/`), ошибки видны;
- [x] keyring: smoke-тест записи/чтения/удаления проходит (реальный Keychain, macOS);
- [x] `l10n` содержит ru и en таблицы (паритет ключей — тестом, en на M9 — P1);
- [x] mock-сервер слушает 24817, отвечает эхом (проверено живым запуском);
- [ ] git: `main` содержит документацию + код, remote запушен, CI зелёный — *локально: 2+ коммита в `main`, remote — после gh-авторизации*;
- [x] ADR-001…003 существуют, CONTRIBUTING §2 поправлен.

---

## 5. Ограничения и риски

| Риск | Мера |
|---|---|
| `gh auth` невалиден | блокер только шага F1.5; всё остальное делается локально до `gh repo create` |
| Нет GPG-ключа | решено D2 — не блокер |
| egui-версия «последняя stable» | зафиксировать exact-версию в `Cargo.toml` при первой сборке, комментарий в lockstep-версии |
| Правки документов (Фаза 0) — большой diff | пошагово, после каждого файла — автопроверка ссылок и CJK-символов |
| Приватный vs public репо | default — private; сменить только по слову пользователя |

## 6. Следующий шаг после M0 → M1

M1 (v0.1 «Нить»): `ld2-protocol` v1 по `docs/protocol.md` целиком, ws-транспорт с reconnect/resync/backoff, мок со сценариями `hello/snapshot/chaos`. План M1 писать по тому же шаблону.
