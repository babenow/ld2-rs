-- M17/F30d: харизма убрана из основных характеристик персонажа.
UPDATE characters SET attrs = attrs - 'cha';
ALTER TABLE characters
    ALTER COLUMN attrs SET DEFAULT '{"str":10,"dex":10,"vit":10,"int":10}';
