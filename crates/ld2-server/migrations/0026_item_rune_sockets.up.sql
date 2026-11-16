-- M18/F58 (ADR-012, уточнение заказчика): число руна-слотов —
-- отдельная колонка определения (как `sockets`), а не поле в `stats`.
-- NULL — не указано → дефолт по типу предмета (броня 0, оружие/посох —
-- все слоты); N — первые N слотов рунные, остальные под самоцветы.
ALTER TABLE item_defs ADD COLUMN rune_sockets INTEGER;
COMMENT ON COLUMN item_defs.rune_sockets IS
  'M18/F58 (ADR-012): сколько слотов принимают руны (NULL = по типу)';
