-- Откат: духовность убирается из дефолта и из строк, где её нет.
ALTER TABLE characters
    ALTER COLUMN attrs SET DEFAULT '{"str":10,"dex":10,"vit":10,"int":10}';
UPDATE characters SET attrs = attrs - 'spir';
