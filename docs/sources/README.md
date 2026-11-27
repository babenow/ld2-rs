# Исходники контента (архив)

Файлы здесь — **источники мержа карт**, а не рантайм-контент: сервер и
редактор читают только `content/locations.json` (см.
`ld2_content::CONTENT_FILES`). Перенесены из `content/` в RF5e, чтобы их
не монтировал прод-контейнер (`docker-compose.yml`: `./content:/content`).

- `locations_main.json` — карта до мержа ад-мира (M25);
- `locations_ad.json` — ад-мир (1702 локации), влит в `locations.json`;
- `locations_renmark.json` — карта Renmark, влита в `locations.json`.

Воспроизводимость мержа: скрипт-импортёр в репозиторий не попал
(REVIEW-M25 §2), файлы сохранены как источник для сверки.
