#!/usr/bin/env python3
"""Импорт дампа локаций PHP-игры → content/locations_ad.json (схема ld2-rs).

Источник:  <ad_game>/1/loc_i2/*      — пофайловый мир (JSON на локацию)
           <ad_game>/1/lang/*        — локализация (loc_title_*/loc_exit_*,
                                        npc_name_*/item_name_*)
           <ad_game>/1/npcCHANGED/*  — статы NPC (char/war)
           <ad_game>/1/itemsCHANGED/*— имена предметов

Приём маппинга (решение по дампу loc_i2):
  * id локации        = имя файла (x2489x471, c.1.gate, …)
  * d[0] (loc_title_) → name (перевод; фолбэки — TITLE_OVERRIDES)
  * d[1] режим        → flags: 0→[], 1/4→["safe"], 2/3→["pvp"]
                        (некорректное «1на юг» нормализуется в 1)
  * d[2::2]/d[3::2]   → exits[{id,label,dir,to}]; выходы с несуществующей
                        целевой локацией отбрасываются (учитываются в отчёте)
  * i.u.*  (игроки)   → пропуск, это runtime-состояние
  * i.n.*  (живые NPC)→ entities kind=monster (n.c./n.a.) или npc
  * t n.*  (таймеры)  → entities + respawn «min:max» из таймера
  * i.i.s.* (объекты) → resource (rudnik/tree/trava — по известной таблице)
                        либо object; имя из lang/backup
  * t i.*  (спавн лута)→ object (resource — только если def есть в items.json)
  * t с произвольным PHP (eval) → пропуск

Сущности получают id "<локация>:<оригинал>" — в ld2-rs mobs/resources/
entity_respawn живут в глобальных HashMap по e.id, коллизии из дампа
(i.s.rudnik в 13 локациях) недопустимы.

Выход: {"locations":[…]} — схема ContentLocation (crates/ld2-server/src/content.rs).
Файл НЕ подхватывается сидом автоматически: чтобы мир заработал, он должен
быть переименован/слит в content/locations.json.
"""
from __future__ import annotations

import collections
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = "/Users/maksim/Downloads/ad_game/1"
LOC_DIR = os.path.join(SRC, "loc_i2")
LANG_MAIN = os.path.join(SRC, "lang", "loc_ru.dat")
LANG_BIG = os.path.join(SRC, "lang", "backup", "ru", "ru-2024-05-08_19-26-04.dat")
ITEMS_DIR = os.path.join(SRC, "itemsCHANGED")
NPC_DIR = os.path.join(SRC, "npcCHANGED")
OUT = os.path.join(HERE, "..", "content", "locations_ad.json")

# Ключи, отсутствующие в lang-файлах дампа.
TITLE_OVERRIDES = {"loc_title_gladiator_arena": "Гладиаторская арена"}
EXIT_OVERRIDES = {
    f"loc_exit_dver_v_komnatu_{i}": f"дверь в комнату {i}" for i in range(1, 5)
}

# Направление по ключу выхода (loc_exit_na_sever → north).
DIR_BY_KEY = (
    ("sever", "north"),
    ("yug", "south"),
    ("vostok", "east"),
    ("zapad", "west"),
)

# Статичные i.s.*-узлы, для которых в items.json ld2 есть ресурс-деф.
STATIC_RESOURCES = {
    "i.s.rudnik": {
        "resource": "ore_copper",
        "profession": "mining",
        "tool": "pickaxe",
    },
    "i.s.tree": {
        "resource": "wood_oak",
        "profession": "woodcutting",
        "tool": "axe",
    },
    "i.s.trava": {
        "resource": "herb",
        "profession": "herbalist",
        "tool": "sickle",
    },
}

# Спавнимый лутом i.*-предмет, у которого есть ресурс-деф в items.json.
TIMER_RESOURCES = {
    "i.f.healherb": {
        "resource": "herb",
        "profession": "herbalist",
        "tool": "sickle",
    },
}

VALID_KINDS = {"player", "monster", "npc", "corpse", "object", "resource", "container"}


def load_json(path):
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def load_lang() -> dict:
    lang = {}
    for path in (LANG_BIG, LANG_MAIN):  # main перекрывает big для loc_*
        if os.path.exists(path):
            lang.update(load_json(path))
    lang.update(TITLE_OVERRIDES)
    lang.update(EXIT_OVERRIDES)
    return lang


def nums(text: str) -> list[int]:
    return [int(x) for x in re.findall(r"\d+", text or "")]


def humanize(key: str) -> str:
    for prefix in ("loc_title_", "loc_exit_", "npc_name_", "item_name_"):
        if key.startswith(prefix):
            key = key[len(prefix):]
            break
    return key.replace("_", " ").strip().capitalize() or key


class Resolver:
    """Перевод ключей lang + каскадные фолбэки для NPC/предметов."""

    def __init__(self, lang: dict):
        self.lang = lang
        self.missing: set[str] = set()

    def key(self, key: str) -> str:
        if key in self.lang:
            return self.lang[key]
        self.missing.add(key)
        return humanize(key)

    def npc_name(self, npc_id: str) -> str:
        # n.c.wolf.dl → npc_name_n_c_wolf_dl, иначе срезаем хвостовые
        # сегменты (n.c.wolf.dl → n.c.wolf, в lang обычно без суффикса).
        cur = npc_id
        while True:
            k = "npc_name_" + cur.lower().replace(".", "_")
            if k in self.lang:
                return self.lang[k]
            if "." not in cur:
                break
            cur = cur.rsplit(".", 1)[0]
        self.missing.add("npc_name_" + npc_id.lower().replace(".", "_"))
        # Последний осмысленный сегмент: n.c.wolf.dl → Wolf.
        tail = re.sub(r"\d+$", "", npc_id.rsplit(".", 1)[-1])
        return humanize(tail) if tail else npc_id

    def item_name(self, item_id: str, first_field: str = "") -> str:
        # В статичных i.*-записях первое поле — уже готовый ключ item_name_*.
        if first_field.startswith("item_name_"):
            k = first_field
            if k in self.lang:
                return self.lang[k]
            self.missing.add(k)
            return humanize(k)
        # У спавнового лута ключ лежит в itemsCHANGED/<id>.
        path = os.path.join(ITEMS_DIR, item_id)
        if os.path.exists(path):
            with open(path, encoding="utf-8") as fh:
                first = fh.read().split("|", 1)[0].strip()
            if first.startswith("item_name_"):
                if first in self.lang:
                    return self.lang[first]
                self.missing.add(first)
                return humanize(first)
        if first_field:
            return first_field  # сырое имя (i.flag: «lyderio vėliava»)
        self.missing.add("item_name_" + item_id.replace(".", "_"))
        return humanize(item_id.rsplit(".", 1)[-1])


def npc_stats(npc_id: str) -> tuple[int, int, int] | None:
    """(hp, max_hp, damage) из npcCHANGED/<id>; None — файла нет."""
    cur = npc_id
    while True:
        path = os.path.join(NPC_DIR, cur)
        if os.path.exists(path):
            with open(path, encoding="utf-8") as fh:
                text = fh.read()
            m = re.search(r"'char'\s*=>\s*'([^']*)'", text)
            hp = max_hp = None
            if m:
                f = m.group(1).split("|")
                if len(f) > 2:
                    a, b = nums(f[1])[:1], nums(f[2])[:1]
                    hp, max_hp = (a[0] if a else None), (b[0] if b else None)
            m = re.search(r"'war'\s*=>\s*'([^']*)'", text)
            damage = None
            if m:
                f = m.group(1).split("|")
                if len(f) > 2:
                    d = nums(f[1])[:1] + nums(f[2])[:1]
                    if len(d) == 2:
                        damage = max(1, round((d[0] + d[1]) / 2))
            if hp and max_hp:
                return int(hp), int(max_hp), damage
            return None
        if "." not in cur:
            return None
        cur = cur.rsplit(".", 1)[0]


def clamp_hp(cur: int, mx: int) -> tuple[int, int]:
    mx = max(1, mx)
    return min(mx, max(1, cur)), mx


def exit_dir(label_key: str) -> str | None:
    for needle, direction in DIR_BY_KEY:
        if needle in label_key:
            return direction
    return None


def main() -> int:
    if not os.path.isdir(LOC_DIR):
        print(f"нет каталога-источника: {LOC_DIR}", file=sys.stderr)
        return 1

    lang = load_lang()
    res = Resolver(lang)

    files = sorted(
        f
        for f in os.listdir(LOC_DIR)
        if os.path.isfile(os.path.join(LOC_DIR, f)) and not f.startswith(".")
    )
    raw: dict[str, dict] = {}
    for name in files:
        try:
            raw[name] = load_json(os.path.join(LOC_DIR, name))
        except json.JSONDecodeError as exc:
            print(f"! битый JSON {name}: {exc}", file=sys.stderr)
            return 1

    report = collections.Counter()
    locations = []

    for loc_id in sorted(raw):
        doc = raw[loc_id]
        parts = (doc.get("d") or "").split("|")
        if len(parts) < 2:
            report["битая строка d"] += 1
            continue

        # ── name / flags ────────────────────────────────────────────
        name = res.key(parts[0])
        mode_m = re.match(r"\d+", parts[1])
        mode = int(mode_m.group()) if mode_m else 0
        flags = {0: [], 1: ["safe"], 4: ["safe"], 2: ["pvp"], 3: ["pvp"]}.get(mode, [])

        # ── exits ───────────────────────────────────────────────────
        exits = []
        pairs = list(zip(parts[2::2], parts[3::2]))
        if len(parts) > 2 and len(parts) % 2 == 1:
            report["выходов без цели (нечётная строка d)"] += 1
        for i, (label_key, target) in enumerate(pairs):
            if target not in raw:
                report["висячих выходов отброшено"] += 1
                continue
            exits.append(
                {
                    "id": f"e{i}",
                    "label": res.key(label_key),
                    **({"dir": exit_dir(label_key)} if exit_dir(label_key) else {}),
                    "to": target,
                }
            )

        # ── entities ────────────────────────────────────────────────
        entities: dict[str, dict] = {}

        def put(orig_id: str, entity: dict, respawn: tuple[int, int] | None):
            """merge: таймер дополняет respawn уже существующей сущности."""
            existing = entities.get(orig_id)
            if existing is None:
                if respawn:
                    entity["respawn"] = {"min_s": respawn[0], "max_s": respawn[1]}
                entities[orig_id] = entity
                return
            if respawn:
                old = existing.get("respawn", {})
                lo = min(respawn[0], old.get("min_s", respawn[0]))
                hi = max(respawn[1], old.get("max_s", respawn[1]))
                existing["respawn"] = {"min_s": lo, "max_s": hi}

        def monster_hp(entity_id: str, char_field: str | None) -> tuple[int, int] | None:
            if char_field:
                f = char_field.split("|")
                if len(f) > 2:
                    a, b = nums(f[1])[:1], nums(f[2])[:1]
                    if a and b:
                        return clamp_hp(a[0], b[0])
            st = npc_stats(entity_id)
            if st:
                return clamp_hp(st[0], st[1])
            report["мобов без hp (дефолт 10)"] += 1
            return (10, 10)

        def monster_damage(entity_id: str, war_field: str | None) -> int | None:
            if war_field:
                f = war_field.split("|")
                if len(f) > 2:
                    d = nums(f[1])[:1] + nums(f[2])[:1]
                    if len(d) == 2:
                        return max(1, round((d[0] + d[1]) / 2))
            st = npc_stats(entity_id)
            return st[2] if st and st[2] else None

        # живые сущности из i.*
        for orig_id, val in (doc.get("i") or {}).items():
            if orig_id.startswith("u."):
                report["игроков пропущено"] += 1
                continue
            if orig_id.startswith("n."):
                kind = "monster" if orig_id.startswith(("n.c.", "n.a.")) else "npc"
                char = val.get("char") if isinstance(val, dict) else None
                war = val.get("war") if isinstance(val, dict) else None
                name_key = (char or "").split("|")[0]
                entity = {
                    "id": f"{loc_id}:{orig_id}",
                    "kind": kind,
                    "name": res.key(name_key) if name_key.startswith("npc_name_")
                    else res.npc_name(orig_id),
                }
                if kind == "monster":
                    hp = monster_hp(orig_id, char)
                    if hp:
                        entity["hp"] = {"cur": hp[0], "max": hp[1]}
                    dmg = monster_damage(orig_id, war)
                    if dmg:
                        entity["damage"] = dmg
                put(orig_id, entity, None)
                report[f"сущностей kind={kind} (из i)"] += 1
                continue
            if orig_id.startswith("i."):
                fields = (val or "").split("|") if isinstance(val, str) else [""]
                entity = {
                    "id": f"{loc_id}:{orig_id}",
                    "name": res.item_name(orig_id, fields[0]),
                }
                cnt = nums(fields[1])[:1] if len(fields) > 1 else []
                respawn_s = nums(fields[3])[:1] if len(fields) > 3 else []
                if orig_id in STATIC_RESOURCES:
                    entity["kind"] = "resource"
                    entity.update(STATIC_RESOURCES[orig_id])
                    entity["max"] = cnt[0] if cnt and cnt[0] > 0 else 3
                    entity["min_rank"] = 1
                    entity["despawn_when_empty"] = entity["max"] == 1
                    if respawn_s and respawn_s[0] > 0:
                        entity["respawn"] = {
                            "min_s": respawn_s[0],
                            "max_s": respawn_s[0],
                        }
                else:
                    entity["kind"] = "object"
                put(orig_id, entity, None)
                report[f"сущностей kind={entity['kind']} (из i)"] += 1
                continue
            report[f"неизвестный префикс id в i: {orig_id[:4]}"] += 1

        # отложенный спавн из t
        for _, val in (doc.get("t") or {}).items():
            if not isinstance(val, str):
                report["таймеров-массивов пропущено"] += 1
                continue
            if not val.startswith(("n.", "i.")):
                report["eval-таймеров пропущено"] += 1
                continue
            f = val.split("|")
            orig_id = f[0]
            if orig_id.startswith("n."):
                rr = nums(f[1]) if len(f) > 1 else []
                respawn = (rr[0], rr[1] if len(rr) > 1 else rr[0]) if rr else None
                kind = "monster" if orig_id.startswith(("n.c.", "n.a.")) else "npc"
                if orig_id in entities:
                    put(orig_id, {}, respawn)
                    continue
                entity = {
                    "id": f"{loc_id}:{orig_id}",
                    "kind": kind,
                    "name": res.npc_name(orig_id),
                }
                if kind == "monster":
                    hp = monster_hp(orig_id, None)
                    entity["hp"] = {"cur": hp[0], "max": hp[1]}
                    dmg = monster_damage(orig_id, None)
                    if dmg:
                        entity["damage"] = dmg
                put(orig_id, entity, respawn)
                report[f"сущностей kind={kind} (из t)"] += 1
            else:  # i.* — спавн предмета на землю
                rr = nums(f[3]) if len(f) > 3 else []
                respawn = (rr[0], rr[1] if len(rr) > 1 else rr[0]) if rr else None
                if orig_id in entities:
                    put(orig_id, {}, respawn)
                    continue
                entity = {
                    "id": f"{loc_id}:{orig_id}",
                    "name": res.item_name(orig_id),
                }
                if orig_id in TIMER_RESOURCES:
                    entity["kind"] = "resource"
                    entity.update(TIMER_RESOURCES[orig_id])
                    entity["max"] = 1
                    entity["min_rank"] = 1
                    entity["despawn_when_empty"] = True
                else:
                    # предметного дефа в items.json нет → видимый объект
                    entity["kind"] = "object"
                    report["лутовых i.* без предметного дефа (→ object)"] += 1
                put(orig_id, entity, respawn)
                report[f"сущностей kind={entity['kind']} (из t-i)"] += 1

        locations.append(
            {
                "id": loc_id,
                "name": name,
                "flags": flags,
                "text": "",
                "exits": exits,
                "entities": list(entities.values()),
            }
        )

    # ── валидация перед записью ────────────────────────────────────
    errors = []
    seen_loc: set[str] = set()
    seen_ent: set[str] = set()
    loc_ids = {l["id"] for l in locations}
    for loc in locations:
        if loc["id"] in seen_loc:
            errors.append(f"дубль локации {loc['id']}")
        seen_loc.add(loc["id"])
        if not loc["name"].strip():
            errors.append(f"пустое имя у {loc['id']}")
        for ex in loc["exits"]:
            if ex["to"] not in loc_ids:
                errors.append(f"{loc['id']}: выход {ex['id']} → неизвестна {ex['to']}")
        for ent in loc["entities"]:
            if ent["id"] in seen_ent:
                errors.append(f"дубль entity id {ent['id']}")
            seen_ent.add(ent["id"])
            if ent["kind"] not in VALID_KINDS:
                errors.append(f"{ent['id']}: недопустимый kind {ent['kind']}")
            if not ent.get("name", "").strip():
                errors.append(f"{ent['id']}: пустое имя")
            hp = ent.get("hp")
            if hp and not (0 < hp["cur"] <= hp["max"]):
                errors.append(f"{ent['id']}: битый hp {hp}")

    if errors:
        for e in errors[:50]:
            print(f"! {e}", file=sys.stderr)
        print(f"валидация провалена: {len(errors)} ошибок", file=sys.stderr)
        return 1

    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8") as fh:
        json.dump({"locations": locations}, fh, ensure_ascii=False, indent=1)
        fh.write("\n")

    # ── отчёт ───────────────────────────────────────────────────────
    print(f"ok → {os.path.relpath(OUT)}")
    print(f"  локаций:            {len(locations)}")
    print(f"  сущностей всего:    {sum(len(l['entities']) for l in locations)}")
    for k in sorted(report):
        print(f"  {k}: {report[k]}")
    if res.missing:
        print(f"  ненайденных ключей lang: {len(res.missing)}")
        for k in sorted(res.missing)[:10]:
            print(f"    - {k}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
