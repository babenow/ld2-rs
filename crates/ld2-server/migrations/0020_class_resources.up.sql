-- M17/F30e: ресурсы по классам (WoW): воин — ярость с нуля,
-- лучник — полная энергия; маг не меняется (мана — дефолт колонок).
-- Максимумы дублируют [gameplay.resource] (источник истины — конфиг;
-- миграция выравнивает существующие строки).
UPDATE characters SET mp_max = 100, mp_cur = 0 WHERE profile = 'warrior';
UPDATE characters SET mp_max = 100, mp_cur = 100 WHERE profile = 'archer';
