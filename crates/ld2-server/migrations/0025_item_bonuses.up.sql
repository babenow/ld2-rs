-- M18/F47 (ADR-010): бонусы предметов — атрибут/параметр/навык.
-- item_defs  — бонусы из контента (content/items.json → bonuses);
-- items      — бонусы инстанса (будущие источники в игре: кузница и т.п.).
-- Формат: [{kind: attr|param|skill, id, value, pct?}].
ALTER TABLE item_defs ADD COLUMN bonuses JSONB;
ALTER TABLE items ADD COLUMN bonuses JSONB;
