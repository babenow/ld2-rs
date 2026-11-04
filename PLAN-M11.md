# План M11 — ld2-rs «Клиент на Ely: всплывающие диалоги» (v0.11.0)

**Статус:** в работе (с 2026-11-04). **Ветка интеграции:** `develop` (git flow), рабочая ветка `feature/M11-ely`.
**Прогресс:** план записан; M11.1 (перевод gpui на `gpui-pre` + каркас Ely) — начата.
**Источник:** запрос — «сделай интерфейс на Ely со всплывающими диалогами». Уточнено:
мигрируем `ld2-client-gpui` на месте, полный набор компонентов Ely, новая веха M11 → v0.11.0,
CI (3 ОС) — обязательный гейт каждой фазы.

**Ely (проверено 2026-11-04):** компонентная библиотека для gpui — `ely-gpui-component 0.1.1`,
`ely-core 0.2.0` (crates.io, MIT, github.com/ELY-Open-Source/Ely-GPUI-Components, elygpui.com).
Нужное нам: глава 09 Overlays (`Dialog`, `Popover`, `HoverCard`, `Lightbox`) и 01 Primitives
(`Portal`, `Overlay`, `Backdrop`-скрим, `Tooltip`); формы (`Input`, `PasswordInput`, `NumberInput`,
`SearchInput`, `Select`, `Combobox`, `HotkeyInput`); `Tabs`, `Table`, `ContextMenu`, `Toast`;
тема light/dark с токенами; `init` (шрифты+тема+Tab-фокус). Pин: `gpui-pre = "=0.3.10"`.

**Решения (согласованы, см. ADR-004):**
- **D15** — миграция **на месте**: `ld2-client-gpui` перерабатывается на Ely; бинарь `ld2-gpui`
  и артефакты `release.yml` сохраняются; egui-клиент не трогаем; третий клиент не заводим.
- **D16** — **пересматривает D11**: источник gpui — crates.io-снапшот `gpui-pre =0.3.10`
  (+ `gpui-pre-platform =0.3.10`, features `font-kit/wayland/x11`); git-пин Zed `v1.26.2` уходит,
  вместе с ним `[patch]` и стаб `vendor/gpui_web` (конфликт `unicode-properties` больше не актуален).
- **D17** — **полный набор Ely** по всем экранам; UI-логика остаётся в `ld2-core`; наш l10n
  (Key, ru/en) сохраняется — i18n Ely не используем; палитра §7.4 ложится на токены темы Ely;
  PT Sans — через `Assets::before`.
- **D18** — веха **M11**, версия **0.11.0** (lockstep D14), тег `m11-done`; старт — только после
  закрытия M10 и релиза v0.10.0 (см. Предусловия); **гейт каждой фазы — CI зелёный на 3 ОС**
  (Ely авторами на Windows/Linux не тестировалась — проверяем мы).

**DoD M11:** egui-клиент не сломан (тесты зелёные); все модалки gpui-клиента — **всплывающие
диалоги поверх контента** (фон виден; ФР-06 — закрытие только подтверждением); чек-лист
паритета PLAN-M10 обновлён: отклонения «modals replace content», «нет тултипов», «пароль не
маскируется», «combobox → ряды кнопок», «нет stick-to-bottom» — сняты; E2E-тест вехи
(создание персонажа через Dialog → мир → смерть-Dialog → лут-Dialog → инвайт-Dialog);
CI 3 ОС зелёный; артефакты `ld2-gpui` в релизе v0.11.0.

## Предусловия (порядок работ)

1. **Биллинг GitHub Actions** — поднять Spending limits (репо приватное: 2000 мин/мес, macOS ×10).
   Без этого M11 не мёржится (гейт D18).
2. **Закрыть M10**: CI PR #18 → merge → rebase `feature/M10-game` на develop → PR M10.4–M10.6 →
   merge → релиз v0.10.0 (ритуал: `release start` → bump → `--no-ff` в main → tag → develop → `m10-done`).
3. `feature/M11-ely` пинируется на develop **после** в0.10.0 (стековая ветка допускается до
   мержа, как в M10.4–M10.6).

## Фазы (каждая = отдельный PR)

| # | Фаза | Содержание | Критерий |
|---|---|---|---|
| **M11.1** | **P0: gpui-pre + каркас Ely** | Замена источника gpui (`=0.3.10`), удаление `[patch]`/`vendor/gpui_web`; bootstrap: `application().with_assets(Assets)` + `ely_core::init(cx)` + `Theme::set_mode` из config; волна компиляционных фиксов (~5,5k LOC: TextInput, keybindings, hover/StyleRefinement, Entity, scroll); **гейт: CI 3 ОС + смоук** | Окно входа стартует, 140+ тестов зелёные, `cargo tree -d` без двойного gpui |
| **M11.2** | **P1: всплывающие диалоги** | Стек оверлеев поверх root-контента (`Dialog` + `Backdrop`/`Portal`); перевод всех модалок: смерть, осмотр, лут, предмет, улучшение, инкрустация, инвайты (группа/гильдия), сделка, создание/удаление персонажа, запись хоткея; Escape/клик-по-скриму — закрытие, кроме ФР-06 | Диалог не заменяет экран; e2e-сценарий death→loot с фоном |
| **M11.3** | **P2: формы** | `PasswordInput` (маска), `Input` submit-Enter (наш `text_input.rs` удаляется/shim), `NumberInput` золото сделки, `SearchInput` фильтр чата, `Select`/`Combobox` (профессии/канал/предметы), `HotkeyInput` ремаппинг ФР-19 | Отклонения «пароль», «combobox», «хоткеи» — сняты |
| **M11.4** | **P3: навигация/меню/фидбек** | `Tabs` (лист, чат), `ContextMenu` (ПКМ сущности/предметы), `Tooltip` (предмет/хотбар/цель), `Toast` (notice/ошибки связи), `ScrollArea` stick-to-bottom (журнал/чат) | Отклонения «тултипы», «stick-to-bottom» — сняты |
| **M11.5** | **P4: тема/шрифт/паритет** | Тема Ely light/dark ↔ config; §7.4 → токены; `font_scale` через rem (проверить; нет — остаётся задокументированным); PT Sans `Assets::before`; пересъёмка чек-листа «ФР × egui × Ely»; drag&drop инвентаря — решение в ADR (stretch: `Draggable`/`SortableList`) | Чек-лист обновлён; обе темы; кириллица |
| **M11.6** | **P5: DoD и релиз** | E2E-тест вехи, смоук обоих клиентов, CHANGELOG/ROADMAP/PLAN статусы, release/0.11.0 → `m11-done`, артефакты | DoD M11 (см. выше) |

**Отклонения M10 → закрытие в M11:** modals-replace-content → M11.2; combobox-ряды → M11.3;
password-маска → M11.3; tooltips → M11.4; stick-to-bottom → M11.4; drag&drop → M11.5 (решение);
font_scale → M11.5 (если gpui-pre даёт rem-масштаб).

## Риски

| Риск | Митигация |
|---|---|
| Ely ранняя (0.1.x, «no crate is stable» в их README) | пин `=0.3.10` + Cargo.lock; фазы маленькими PR; при баге — вендинг `vendor/ely` с локальным патчем |
| Windows/Linux не тестировались авторами Ely | M11.1-гейт CI 3 ОС **до** P1; при блокирующем баге — откат глубины D17 до «только диалоги» (фиксация в ADR-004 amendment) |
| Волна API-ломок gpui v1.26.2 → снапшот 08cbb88 | отдельный PR M11.1; store-логика (140 тестов) не должна пострадать |
| Конфликт версий gpui в графе | `cargo tree -d` в гейте; `=0.3.10` не трогаем до выхода Ely под 0.3.11 |

## Правила

- Один PR = одна фаза; CI 3 ОС зелёный перед squash-merge; DoD фазы — в теле PR.
- Контракт (`ld2-protocol`) и `ld2-core` — без изменений поведения (UI-слой).
- e2e-тест вехи — обязательная часть M11.6.
