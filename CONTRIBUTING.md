# CONTRIBUTING.md — правила разработки ld2-rs

Документ нормативный. Любое отступление — только через ADR (`docs/decisions/`, см. §9 ТЗ) или явное согласование в PR.
Связанные документы: [`docs/tz.md`](./docs/tz.md) — техническое задание и контракт протокола.

---

## 1. Инструменты и общий стек

- **VCS:** git. Репозиторий — единственный источник правды по коду, контракту и истории.
- **Язык/сборка:** Rust, workspace Cargo (`rust-toolchain.toml` фиксирует стабильный toolchain — не менять без ADR).
- **CI:** GitHub Actions (fmt → clippy → test → build, матрица Win/macOS/Linux).
- **Кодинг-конвенции:** `cargo fmt` + `cargo clippy -- -D warnings` — обязательны; стиль именования модулей/фич — по `docs/tz.md` (ФР-xx/NFR-xx ID, snake_case модули, `evt/`–`cmd/` домены).
- **Язык проекта — русский:** все коммиты (subject и body), все комментарии кода (включая doc-комментарии `///`, `//!`), описания PR, ADR и документация — **пишутся на русском языке**.
  - Английский допускается только в структурных идентификаторах и служебных маркерах: `type`/`scope` коммитов (`feat`, `fix`, `client`…), маркеры Conventional Commits (`BREAKING CHANGE:`, `Refs:`, `FR:`, `Milestone:`), имена API/типов/кодов ошибок (`E_…`, `cmd/`, `evt/`, имена crate'ов), а также названия инструментов (git, cargo, CI) и общепринятых терминов без точного эквивалента (reconnect, milestone, snapshot — по желанию можно русифицировать в тексте, идентификаторы — нет).

---

## 2. Инициализация репозитория

Выполняется один раз при заведении проекта (M0):

```bash
git init -b main
# git config commit.gpgsign true       # подпись коммитов — рекомендация (ADR-001), не включается автоматически
git config gitflow.branch.master main   # git-flow: релизная ветка называется main
git flow init -d                        # git flow: main + develop, префиксы feature/ release/ hotfix/
git config pull.rebase true             # локальные обновления develop — через rebase
git remote add origin git@github.com:<org>/ld2-rs.git
```

> **Статус (2026-11-03):** репозиторий инициализирован — git flow активен, рабочая ветка **`develop`**,
> remote `origin` → `github.com/babenow/ld2-rs`.

`.gitignore` обязателен с первого коммита: `target/`, `**/*.rs.bk`, IDE-артефакты (`.idea/`, `.vscode/` кроме общих `settings.json` по согласованию), ОС-мусор (`.DS_Store`), локальные конфиги (`*.local.toml`), логи и кэши клиента (`client-data/`).
`Cargo.lock` — **коммитится** (у нас приложение, не библиотека).

> **Секреты коммитить запрещено:** ключи, токены, `.env*`, `*.pem`, `*.p12` не коммитятся никогда. Тестовые креды мока (`demo/demo123`) — фикстуры, не секреты.

---

## 3. Модель веток: Git Flow

Используем классический **Git Flow** (модель Vincent Driessen / `nvie`, реализация `nvie/git-flow` 0.4.1). Структура:

> **Статус (2026-11-03):** git flow **инициализирован** в репозитории (`git flow init -d`,
> `gitflow.branch.master = main`). **Вся разработка ведётся в ветке `develop`** — это рабочая
> интеграционная ветка проекта (M1 и дальше). `main` обновляется только merge'ем `release/*` или
> `hotfix/*` и всегда соответствует последнему тегу `vX.Y.Z`.

| Ветка | Назначение | Жизненный цикл |
|---|---|---|
| `main` | только релизные состояния, соответствует последнему тегу `vX.Y.Z` | бессрочная, защищённая |
| `develop` | интеграционная ветка текущей разработки | бессрочная, защищённая |
| `feature/<milestone>-<имя>` | фичи по §10 ТЗ | от `develop`, сливается обратно, удаляется |
| `release/<X.Y.Z>` | стабилизация релиза | от `develop`, сливается в `main` и `develop`, удаляется |
| `hotfix/<X.Y.(Z+1)>` | срочное исправление релиза | от `main`, сливается в `main` и `develop`, удаляется |
| `chore/<имя>` | инфраструктура: CI, конфиг, зависимости | от `develop`, удаляется после merge |
| `docs/<имя>` | правки ТЗ `[tz-amend]`, ADR, руководства | от `develop` |

### 3.1 Правила ветвления

1. **`main` и `develop` защищены:** изменения только через PR (см. §6). Push-force запрещён (`git push --force` на эти ветки — табу).
2. Имя feature-ветки: `feature/M3-location-panel`, `feature/M6-crime-statuses` — префикс milestone из §10 ТЗ обязателен, если фича относится к milestone.
3. Ветки `feature/*` не живут дольше одного milestone: большие задачи дробятся (`feature/M4-inventory-drag` → `feature/M4-loot-tosts`).
4. Ветвление только от актуального `develop` (после `git checkout develop && git pull --rebase`).
5. Для каждой ветки — одна тема; смешанные PR («фича + рефакторинг CI») не ревьюятся, возвращаются.

### 3.2 Рабочий процесс (шпаргалка)

```bash
# новая фича
git checkout develop && git pull --rebase
git checkout -b feature/M4-inventory-drag
# ...коммиты...
git push -u origin feature/M4-inventory-drag        # → PR в develop

# актуализация фичи под develop
git checkout feature/M4-inventory-drag
git fetch && git rebase origin/develop              # только rebase, без merge-коммитов истории

# релиз
git checkout -b release/0.3.0 develop
#   bump версий (см. §5), CHANGELOG → «Released»
git checkout main && git merge --no-ff release/0.3.0
git tag -s v0.3.0 -m "релиз 0.3.0"
git checkout develop && git merge --no-ff release/0.3.0
git push origin main develop --follow-tags
git branch -d release/0.3.0

# hotfix
git checkout -b hotfix/0.3.1 main
#   fix + bump patch + тег
git checkout main && git merge --no-ff hotfix/0.3.1 && git tag -s v0.3.1
git checkout develop && git merge hotfix/0.3.1
```

---

## 4. Коммиты: Conventional Commits

**Язык коммитов:** subject, body, футеры и аннотации тегов — на русском языке (см. §1).
Английскими остаются только `type`, `scope` и служебные маркеры — это обязательные ключи формата.

```
<type>(<scope>): <subject, imperative mood, на русском, ≤ 72 chars>

[body: что и почему, на русском, ≤ 100 cols/line]

[footer: Tracing, ADR, tz-amend, BREAKING CHANGE — ключи на английском, содержание на русском]
```

- **type:** `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `style`, `build`, `ci`, `chore`, `revert`.
- **scope (обязателен):** `protocol` (`ld2-protocol`), `client` (общий UI/логика), `transport`, `reducer`, `ui`, `mock`, `build`, `ci`, `docs`.
- **Breaking:** `type!` или `!` в scope + `BREAKING CHANGE:` в футере. Правку контракта протокола считать breaking, только если поднимается `v` конверта (§4.11 ТЗ) — иначе это additive capability.
- **Связь с ТЗ:** в footer указывать `FR: ФР-11`, `Milestone: M6`, при изменении контракта/структуры — префикс subject `[tz-amend]` (требование §15.1 ТЗ).

Примеры (subject/body — по-русски):

```
feat(client): бейджи статусов преступлений с severity

FR: ФР-11
Milestone: M6
```

```
[tz-amend] docs(protocol): добавить evt/socket/extract в таблицу §4.7

Refs: ADR-004, FR: ФР-09
```

Запрещено: коммиты «wip», «fix», «stuff», merge-коммиты от `git pull` (реbase обязателен), коммит в `main/develop` напрямую.

---

## 5. Версионирование и теги

### 5.1 Приложение и crate-ы — SemVer 2.0, lockstep

Все crate-ы workspace (`ld2-protocol`, `ld2-client`, `ld2-mock-server`) версионируются **единой версией** `X.Y.Z` во всех `Cargo.toml` — синхронно, bump только на `release/*`-ветке одним коммитом `chore(build): релиз v0.3.0`.

| Изменение | Bump |
|---|---|
| Ломает UX/совместимость сохранённых конфигов, протокол `v` меняется | **major** (`X+1.0.0`) |
| Новая игровая фича / новые команды-события (additive) / новые capability | **minor** (`Y+1.0`) |
| Исправления, внутренние рефакторинги, балансировочные значения мока | **patch** (`Z+1`) |

- `0.x.y` — пре-1.0: minor считается «стабильность фичи», major стартует с релизом M9 (клиент v1.0.0 для публичного доступа).
- Теги релизов: `v<X.Y.Z>` (annotated; крипто-подпись — по желанию, ADR-001). Пре-релизы: `v0.4.0-rc.1`.

### 5.2 Версия протокола — отдельно от версии приложения

В конверте сообщений `v` (§4.3 ТЗ) — это **wire-версия контракта**, она живёт по своим правилам и НЕ равна semver приложения. Изменение wire-`v` возможно только major-bump'ом приложения и оформляется PR `[tz-amend]` с обновлением §4 ТЗ и golden-тестов до кода.

### 5.3 Milestone-теги

По §10 ТЗ каждый завершённый milestone тегируется: `m<n>-done` на merge-коммите `develop` (например `m3-done`). Это контрольные точки, не релизы.

### 5.4 CHANGELOG

`CHANGELOG.md` — формат **Keep a Changelog**, секции по типам коммитов (`Added / Changed / Fixed / Removed` — служебные названия по стандарту формата, содержание записей — **на русском**), записи ведутся с первого коммита milestone через commit footer или из `git log` релиз-прогона. Заголовок незарелизенной секции: `## [Unreleased]`.

---

## 6. Pull Request и ревью

1. PR в `develop` (feature/chore/docs) или в `main`+`develop` (release/hotfix — по процедуре §3.2).
2. Обязательные поля описания PR (текст описания — **на русском языке**):
   - `Ties to:` ФР/NFR/§ ТЗ; milestone;
   - What/Why/How — кратко;
   - Checklist: `fmt+clippy+test locally`, unit-тесты редукторов на новые домены, e2e-прогон мока (для геймплейных фич), UI-фича без reducer-теста **не мёржится** (§15.5 ТЗ).
3. Минимум 1 одобрение (2 для `protocol`-crate и `[tz-amend]`).
4. Squash-merge в `develop` (сохраняем атомарность milestone-шагов), `--no-ff` merge в `main` (история релизов явная).
5. CI green — блокирующее; подписи коммитов/тегов — рекомендация (ADR-001).

---

## 7. Локальные хуки и проверки

Рекомендуемые pre-commit проверки (CI дублирует и является gate):

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Хуки не обязательны к настройке, но CI не пускает нарушителя. Подключаем через `git config core.hooksPath .githooks` (папка `.githooks/` с `pre-commit` скриптом выше) по выбору контрибьютора.

---

## 8. Ветвление истории: чего не делать

- Не переписывать историю (`rebase -i`, `filter-branch`) в `main/develop` после push — только в своих feature-ветках до merge.
- Не создавать merge-коммиты `Merge branch 'develop' into feature/...` — актуализация строго rebase.
- Не коммитить артефакты сборки (`target/`, `.dmg`, `.AppImage`, `.exe`) — только через GitHub Releases CI-прогона (§12 ТЗ).
- Не менять `rust-toolchain.toml`, структуру §9 ТЗ, `docs/tz.md` без ветки `docs/…` + `[tz-amend]` + 2 approvals.

---

## 9. Соответствие milestone-плану

См. таблицу §10 ТЗ: milestone → ветки → PR → tag `m<n>-done` → DoD.
Начальный коммит M0 должен содержать: `Cargo.toml` workspace, `rust-toolchain.toml`, `.gitignore`, `README.md`, `CONTRIBUTING.md`, `ROADMAP.md`, `docs/tz.md`, `docs/protocol.md`, `docs/skills.md`, CI-воркфлоу.

---

## 10. Ссылки

- Git Flow (nvie): <https://nvie.com/posts/a-successful-git-branching-model/>
- Conventional Commits 1.0: <https://www.conventionalcommits.org/>
- Semantic Versioning 2.0: <https://semver.org/>
- Keep a Changelog 1.1: <https://keepachangelog.com/>
