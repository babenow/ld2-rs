# ADR-004: Клиент на Ely — gpui-pre 0.3.10 вместо git-пина Zed; всплывающие диалоги

**Дата:** 2026-11-04 · **Статус:** принято · **Пересматривает:** D11 (PLAN-M10) · **Связано:** `PLAN-M11.md` D15–D18; `PLAN-M10.md` «Отклонения», чек-лист паритета

## Контекст

Запрос: интерфейс на Ely со всплывающими диалогами. gpui-клиент M10 реализован на goлом gpui
(git-пин Zed `v1.26.2`, D11) и накопил задокументированные отклонения: модалки заменяют контент
(нет overlay-позиционирования в v1.26.2), нет тултипов, пароль без маски, combobox → ряды кнопок,
нет stick-to-bottom, drag&drop инвентаря заменён меню.

Ely (`ely-gpui-component 0.1.1` / `ely-core 0.2.0`, crates.io, MIT, 26.10.2026) — компонентная
библиотека поверх gpui: Dialog/Backdrop/Portal/Popover/Tooltip, формы (PasswordInput, Select,
Combobox, HotkeyInput, NumberInput, SearchInput), Tabs/Table/ContextMenu/Toast, тема light/dark
с токенами, `init` с фокус-скоупом. Закрывает все перечисленные отклонения.

Ограничение: Ely пинит `gpui = { package = "gpui-pre", version = "=0.3.10" }` — crates.io-снапшот
gpui (zed@08cbb88, публикация 26.10.2026, maintainer huacnlee). Два источника gpui (git Zed и
crates.io gpui-pre) в одной сборке несовместимы.

## Решение

1. **D15 — миграция на месте.** `ld2-client-gpui` перерабатывается на Ely; бинарь `ld2-gpui`
   и артефакты `release.yml` сохраняются; egui-клиент остаётся эталоном паритета; третий клиент
   не заводится (два gpui-источника в workspace = двойная компиляция).
2. **D16 — источник gpui: `gpui-pre = "=0.3.10"` + `gpui-pre-platform = "=0.3.10"`
   (features `font-kit`, `wayland`, `x11`).** Git-пин Zed `v1.26.2` снимается. Удаляются
   `[patch."https://github.com/zed-industries/zed"] gpui_web` и стаб `vendor/gpui_web`:
   crates.io-снапшот не тянет gpui_web, конфликт `unicode-properties` (0.1.3 против ^0.1.4
   у epaint/egui 0.37) исчезает сам.
3. **D17 — полный набор компонентов Ely** по всем экранам; UI-логика и состояние — в `ld2-core`
   без изменений; л10n остаётся наш (`ld2_core::l10n`, ru/en) — i18n Ely не используем;
   палитра §7.4 отображается на токены темы Ely (`Theme::set_mode` из config); шрифты: Ely `Assets`
   + наш PT Sans (кириллица, §13) — при необходимости через `Assets::before`.
4. **D18 — веха M11 → v0.11.0** (lockstep), гейт каждой фазы — CI на 3 ОС, поскольку Ely
   авторами тестировалась на macOS (их CI собирает Linux/Windows, но «builds are not tested yet»
   в смысле прогона).

## Последствия

- Волна компиляционных фиксов по ~5,5k LOC gpui-клиента (снапшот 08cbb88 новее v1.26.2:
  TextInput/keybindings, `Entity::update`, scroll-id-требования, модификаторы Keystroke) —
  вынесена в фазу M11.1 отдельным PR.
- Наш адаптированный `text_input.rs` и «модалки-замены-контента» удаляются за ненадобностью —
  замещаются `Input`/`Dialog` Ely.
- Зависимость от качества ранней библиотеки (API 0.x): пин `=0.3.10`, фиксация в Cargo.lock;
  план Б — вендинг `vendor/ely` с локальными патчами; план В (при блокирующем баге на
  Windows/Linux) — сужение D17 до «только диалоги».
- Контракт (`docs/protocol.md`, `ld2-protocol`) и `ld2-core` не меняются: чистый UI-слой.
- Отклонения PLAN-M10 снимаются: диалоги (M11.2), маска пароля и combobox (M11.3), тултипы
  и stick-to-bottom (M11.4), font_scale и переключение темы (M11.5).
- **Drag&drop инвентаря (ФР-07): остаётся без драг-эн-дропа** — действия по ПКМ-меню
  (ContextMenu Ely, M11.4) дают полный набор egui-операций; Draggable/SortableList Ely
  остаётся в бэклоге (решение M11.5: не раздувать веху).
- **Диспетчер диалогов — в ядре**: `AppStore::active_dialog` (ld2-core) хранит приоритет
  и гейт экрана; UI-диспетчер его читает. E2E-тест вехи (M11.6) гоняет сценарий
  «создание → мир → смерть → лут → инвайт → лист» против той же функции.
