#!/usr/bin/env python3
"""Генератор контента M38: горное дело и самоцветы.

Идемпотентно обновляет:
  * content/items.json   — 6 руд, 32 необработанных камня, 96 обработанных;
  * content/recipes.json — 96 рецептов ювелира (3 версии на камень);
  * content/locations.json — 3 рудника, жилы, ювелирный стол;
  * content/shops.json   — резец у городского старосты.

Запуск:  python3 scripts/gen-mining-content.py
"""

from __future__ import annotations

import json
import os
from typing import Iterable

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CONTENT = os.path.join(ROOT, "content")

# ─── ценности ────────────────────────────────────────────────────────────────
# v1/v2/v3 — чистые; mixed — 60% от чистого (с округлением).
PURE_VALUES = {
    "attr": [2, 4, 7],
    "flat": [2, 3, 5],
    "accuracy": [2, 3, 5],
    "chance": [1, 2, 3],
    "crit_damage": [3, 7, 10],
}
MIXED_VALUES = {
    "attr": [1, 2, 4],
    "flat": [1, 2, 3],
    "accuracy": [1, 2, 3],
    "chance": [1, 1, 2],
    "crit_damage": [2, 4, 6],
}
VERSION_NAMES = ["Гранёный", "Огранённый", "Совершенный"]
VERSION_RARITY = ["uncommon", "rare", "epic"]
VERSION_GOLD = [150, 450, 1200]
VERSION_XP = [40, 70, 110]
VERSION_TIME = [3, 5, 7]

# ─── руды и жилы ─────────────────────────────────────────────────────────────
def vein_respawn(req_level: int) -> tuple[int, int]:
    """M39: респавн жилы растёт с её уровнем: 60+4L .. 120+6L (медь — 60..120)."""
    if req_level <= 1:
        return (60, 120)
    return (60 + 4 * req_level, 120 + 6 * req_level)


ORES = [
    # id, name, rarity, gold, req_level, min_rank, max, respawn, node_prefix
    ("ore_copper", "Медная руда", "common", 6, 1, 1, 3, (60, 120)),
    ("ore_tin", "Оловянная руда", "common", 12, 35, 1, 3, (200, 330)),
    ("ore_iron", "Железная руда", "uncommon", 24, 50, 2, 3, (260, 420)),
    ("ore_bronze", "Бронзовая руда", "uncommon", 36, 75, 2, 3, (360, 570)),
    ("ore_silver", "Серебряная руда", "uncommon", 48, 100, 3, 3, (460, 720)),
    ("ore_gold", "Золотая руда", "rare", 120, 126, 3, 2, (564, 876)),
    ("ore_platinum", "Платиновая руда", "rare", 180, 150, 4, 2, (660, 1020)),
    ("ore_adamantite", "Адамантитовая руда", "epic", 240, 176, 4, 2, (764, 1176)),
    ("ore_mithril", "Мифриловая руда", "epic", 300, 200, 5, 2, (860, 1320)),
]
ORE_BY_ID = {row[0]: row for row in ORES}

MIXED = [
    # key, name, stats[(kind, id, value_category)]
    ("agate", "Агат", [("attr", "str", "attr"), ("attr", "vit", "attr")]),
    ("malachite", "Малахит", [("attr", "dex", "attr"), ("param", "accuracy", "accuracy")]),
    ("obsidian", "Обсидиан", [("attr", "vit", "attr"), ("param", "armor", "flat")]),
    ("amethyst", "Аметист", [("attr", "int", "attr"), ("param", "magic_resist", "chance")]),
    ("topaz", "Топаз", [("param", "accuracy", "accuracy"), ("param", "crit_chance", "chance")]),
    ("ruby", "Рубин", [("param", "damage", "flat"), ("param", "crit_damage", "crit_damage")]),
    ("sapphire", "Сапфир", [("range", "parry", "flat"), ("range", "block", "flat")]),
    ("emerald", "Изумруд", [("param", "dodge", "chance"), ("param", "parry_chance", "chance")]),
    ("onyx", "Оникс", [("range", "block", "flat"), ("param", "armor", "flat")]),
    ("opal", "Опал", [("param", "magic_crit_chance", "chance"), ("param", "magic_defense", "flat")]),
    ("garnet", "Гранат", [("attr", "str", "attr"), ("param", "damage", "flat")]),
    ("aquamarine", "Аквамарин", [("attr", "dex", "attr"), ("param", "dodge", "chance")]),
]

PURE = [
    # key, name, kind, param, category, pct
    ("str", "Пирит", "attr", "str", "attr", False),
    ("dex", "Хризопраз", "attr", "dex", "attr", False),
    ("vit", "Авантюрин", "attr", "vit", "attr", False),
    ("int", "Флюорит", "attr", "int", "attr", False),
    ("accuracy", "Циркон", "param", "accuracy", "accuracy", False),
    ("damage", "Пироп", "param", "damage", "flat", False),
    ("crit_chance", "Хризолит", "param", "crit_chance", "chance", True),
    ("crit_damage", "Гелиодор", "param", "crit_damage", "crit_damage", True),
    ("armor", "Сардоникс", "param", "armor", "flat", False),
    ("dodge", "Кунцит", "param", "dodge", "chance", True),
    ("parry", "Родонит", "range", "parry", "flat", False),
    ("parry_chance", "Родохрозит", "param", "parry_chance", "chance", True),
    ("block", "Диоптаз", "range", "block", "flat", False),
    ("block_chance", "Сердолик", "param", "block_chance", "chance", True),
    ("magic_defense", "Селестин", "param", "magic_defense", "flat", False),
    ("magic_dodge", "Халцедон", "param", "magic_dodge", "chance", True),
    ("magic_resist", "Раухтопаз", "param", "magic_resist", "chance", True),
    ("magic_dispel", "Апофиллит", "param", "magic_dispel", "chance", True),
    ("magic_crit_damage", "Танзанит", "param", "magic_crit_damage", "crit_damage", True),
    ("magic_crit_chance", "Бенитоит", "param", "magic_crit_chance", "chance", True),
]

# ─── пулы жил ────────────────────────────────────────────────────────────────
COPPER_MIXED = ["raw_agate", "raw_malachite", "raw_obsidian", "raw_amethyst"]
COPPER_PURE = ["raw_stone_str", "raw_stone_dex", "raw_stone_vit", "raw_stone_int"]
TIN_MIXED = ["raw_topaz", "raw_ruby", "raw_garnet", "raw_aquamarine"]
TIN_PURE = ["raw_stone_accuracy", "raw_stone_damage"]
IRON_MIXED = ["raw_sapphire", "raw_emerald", "raw_onyx"]
IRON_PURE = [
    "raw_stone_crit_chance",
    "raw_stone_crit_damage",
    "raw_stone_armor",
    "raw_stone_dodge",
]
SILVER_MIXED = ["raw_opal"]
SILVER_PURE = [
    "raw_stone_parry",
    "raw_stone_parry_chance",
    "raw_stone_block",
    "raw_stone_block_chance",
]
GOLD_MIXED = ["raw_agate", "raw_topaz", "raw_sapphire", "raw_opal", "raw_garnet"]
GOLD_PURE = [
    "raw_stone_magic_defense",
    "raw_stone_magic_dodge",
    "raw_stone_magic_resist",
    "raw_stone_magic_dispel",
]
ALL_MIXED = [f"raw_{key}" for key, _, _ in MIXED]
ALL_PURE = [f"raw_stone_{key}" for key, _, _, _, _, _ in PURE]

VEINS = [
    # ore_id, mixed, pure, chance
    ("ore_copper", COPPER_MIXED, COPPER_PURE, 0.025),
    ("ore_tin", TIN_MIXED, TIN_PURE, 0.03),
    ("ore_iron", IRON_MIXED, IRON_PURE, 0.035),
    ("ore_bronze", IRON_MIXED, IRON_PURE, 0.0375),
    ("ore_silver", SILVER_MIXED, SILVER_PURE, 0.04),
    ("ore_gold", GOLD_MIXED, GOLD_PURE, 0.05),
    ("ore_platinum", GOLD_MIXED, GOLD_PURE, 0.05),
    ("ore_adamantite", ALL_MIXED, ALL_PURE, 0.055),
    ("ore_mithril", ALL_MIXED, ALL_PURE, 0.06),
]
VEIN_BY_ORE = {ore: (mixed, pure, chance) for ore, mixed, pure, chance in VEINS}


def bonus(kind: str, id: str, value: int, pct: bool = False) -> dict:
    out = {"kind": kind, "id": id, "value": value}
    if pct:
        out["pct"] = True
    return out


def mixed_bonuses(stats: Iterable[tuple], version: int) -> list[dict]:
    out = []
    for kind, id, category in stats:
        value = MIXED_VALUES[category][version]
        pct = category in ("chance", "crit_damage")
        if kind == "range":
            out.append(bonus("param", f"{id}_min", value))
            out.append(bonus("param", f"{id}_max", value))
        elif kind == "attr":
            out.append(bonus("attr", id, value))
        else:
            out.append(bonus("param", id, value, pct))
    return out


def pure_bonuses(kind: str, param: str, category: str, pct: bool, version: int) -> list[dict]:
    value = PURE_VALUES[category][version]
    if kind == "range":
        return [
            bonus("param", f"{param}_min", value),
            bonus("param", f"{param}_max", value),
        ]
    if kind == "attr":
        return [bonus("attr", param, value)]
    return [bonus("param", param, value, pct and category in ("chance", "crit_damage"))]


def gem_item(def_id: str, name: str, rarity: str, gold: int, bonuses: list[dict]) -> dict:
    return {
        "id": def_id,
        "type": "gem",
        "name": name,
        "rarity": rarity,
        "upgrade": 0,
        "stats": {},
        "bonuses": bonuses,
        "gold_value": gold,
    }


def raw_item(def_id: str, name: str, rarity: str, gold: int, item_type: str) -> dict:
    return {
        "id": def_id,
        "type": item_type,
        "name": name,
        "rarity": rarity,
        "upgrade": 0,
        "stats": {},
        "gold_value": gold,
        "qty": 1,
    }


def generated_items() -> list[dict]:
    items: list[dict] = []
    for ore_id, name, rarity, gold, *_ in ORES:
        items.append(raw_item(ore_id, name, rarity, gold, ore_id))

    for key, name, stats in MIXED:
        raw_id = f"raw_{key}"
        items.append(raw_item(raw_id, f"Необработанный {name.lower()}", "uncommon", 40, raw_id))
        for v in range(3):
            items.append(
                gem_item(
                    f"gem_{key}_{v + 1}",
                    f"{VERSION_NAMES[v]} {name.lower()}",
                    VERSION_RARITY[v],
                    VERSION_GOLD[v],
                    mixed_bonuses(stats, v),
                )
            )

    for key, name, kind, param, category, pct in PURE:
        raw_id = f"raw_stone_{key}"
        items.append(
            raw_item(
                raw_id,
                f"Необработанный {name.lower()}",
                "common",
                25,
                raw_id,
            )
        )
        for v in range(3):
            items.append(
                gem_item(
                    f"gem_stone_{key}_{v + 1}",
                    f"{VERSION_NAMES[v]} {name.lower()}",
                    VERSION_RARITY[v],
                    VERSION_GOLD[v],
                    pure_bonuses(kind, param, category, pct, v),
                )
            )
    return items


def generated_recipes() -> list[dict]:
    recipes: list[dict] = []
    for key, name, _ in MIXED:
        raw_id = f"raw_{key}"
        for v in range(3):
            recipes.append(
                {
                    "id": f"rec_cut_{key}_{v + 1}",
                    "name": f"{VERSION_NAMES[v]} {name.lower()}",
                    "profession": "jeweler",
                    "rank": v + 1,
                    "station": "jeweler_table",
                    "tool": {"type": "chisel"},
                    "requires": {},
                    "inputs": [{"type": raw_id, "qty": v + 1}],
                    "time_s": VERSION_TIME[v],
                    "xp": {"profession": VERSION_XP[v]},
                    "output": {"type": f"gem_{key}_{v + 1}", "qty": 1},
                    "station_label": "Ювелирный стол",
                }
            )
    for key, label, *_ in PURE:
        raw_id = f"raw_stone_{key}"
        for v in range(3):
            recipes.append(
                {
                    "id": f"rec_cut_stone_{key}_{v + 1}",
                    "name": f"{VERSION_NAMES[v]} {name.lower()}",
                    "profession": "jeweler",
                    "rank": v + 1,
                    "station": "jeweler_table",
                    "tool": {"type": "chisel"},
                    "requires": {},
                    "inputs": [{"type": raw_id, "qty": v + 1}],
                    "time_s": VERSION_TIME[v],
                    "xp": {"profession": VERSION_XP[v]},
                    "output": {"type": f"gem_stone_{key}_{v + 1}", "qty": 1},
                    "station_label": "Ювелирный стол",
                }
            )
    return recipes


def vein_node(
    node_id: str,
    name: str,
    ore_id: str,
    max_amount: int,
    respawn: tuple[int, int],
    req_level: int,
    min_rank: int,
    mixed: list[str],
    pure: list[str],
    chance: float,
) -> dict:
    return {
        "id": node_id,
        "kind": "resource",
        "name": name,
        "resource": ore_id,
        "max": max_amount,
        "respawn": {"min_s": respawn[0], "max_s": respawn[1]},
        "despawn_when_empty": False,
        "profession": "mining",
        "min_rank": min_rank,
        "tool": "pickaxe",
        "required_level": req_level,
        "gems": {
            "chance": chance,
            "mixed_share": 0.1,
            "mixed": mixed,
            "pure": pure,
        },
    }


def mine_locations() -> list[dict]:
    copper_mixed, copper_pure, copper_chance = VEIN_BY_ORE["ore_copper"]
    tin_mixed, tin_pure, tin_chance = VEIN_BY_ORE["ore_tin"]
    iron_mixed, iron_pure, iron_chance = VEIN_BY_ORE["ore_iron"]
    bronze_mixed, bronze_pure, bronze_chance = VEIN_BY_ORE["ore_bronze"]
    silver_mixed, silver_pure, silver_chance = VEIN_BY_ORE["ore_silver"]
    gold_mixed, gold_pure, gold_chance = VEIN_BY_ORE["ore_gold"]
    platinum_mixed, platinum_pure, platinum_chance = VEIN_BY_ORE["ore_platinum"]
    adamantite_mixed, adamantite_pure, adamantite_chance = VEIN_BY_ORE["ore_adamantite"]
    mithril_mixed, mithril_pure, mithril_chance = VEIN_BY_ORE["ore_mithril"]
    return [
        {
            "id": "l_mine_copper",
            "name": "Медный карьер",
            "spawn": False,
            "flags": ["pvp"],
            "text": "Ступени карьера уходят вниз; в стенах блестит медная зелень и тусклое олово.",
            "exits": [
                {"id": "e_mine_exit", "label": "выход из карьера", "to": "l_village"},
                {"id": "e_mine_deep", "label": "спуск в железные штольни", "to": "l_mine_iron"},
            ],
            "entities": [
                vein_node("ore_copper_1", "Медная жила", "ore_copper", 3, vein_respawn(1), 1, 1, copper_mixed, copper_pure, copper_chance),
                vein_node("ore_copper_2", "Медная жила", "ore_copper", 3, vein_respawn(1), 1, 1, copper_mixed, copper_pure, copper_chance),
                vein_node("ore_copper_3", "Медная жила", "ore_copper", 3, vein_respawn(1), 1, 1, copper_mixed, copper_pure, copper_chance),
                vein_node("ore_tin_1", "Оловянная жила", "ore_tin", 3, vein_respawn(35), 35, 1, tin_mixed, tin_pure, tin_chance),
                vein_node("ore_tin_2", "Оловянная жила", "ore_tin", 3, vein_respawn(35), 35, 1, tin_mixed, tin_pure, tin_chance),
            ],
        },
        {
            "id": "l_mine_iron",
            "name": "Железные штольни",
            "spawn": False,
            "flags": ["pvp"],
            "text": "Крепи штолен скрипят; в темноте пахнет бронзой и ржавой водой.",
            "exits": [
                {"id": "e_mine_copper", "label": "наверх, в медный карьер", "to": "l_mine_copper"},
                {"id": "e_mine_deep", "label": "спуск в серебряные копи", "to": "l_mine_silver"},
            ],
            "entities": [
                vein_node("ore_iron_1", "Железная жила", "ore_iron", 3, vein_respawn(50), 50, 2, iron_mixed, iron_pure, iron_chance),
                vein_node("ore_iron_2", "Железная жила", "ore_iron", 3, vein_respawn(50), 50, 2, iron_mixed, iron_pure, iron_chance),
                vein_node("ore_iron_3", "Железная жила", "ore_iron", 3, vein_respawn(50), 50, 2, iron_mixed, iron_pure, iron_chance),
                vein_node("ore_bronze_1", "Бронзовая жила", "ore_bronze", 3, vein_respawn(75), 75, 2, bronze_mixed, bronze_pure, bronze_chance),
                vein_node("ore_bronze_2", "Бронзовая жила", "ore_bronze", 3, vein_respawn(75), 75, 2, bronze_mixed, bronze_pure, bronze_chance),
            ],
        },
        {
            "id": "l_mine_silver",
            "name": "Серебряные копи",
            "spawn": False,
            "flags": ["pvp"],
            "text": "Белые прожилки серебра и жёлтые искры золота в чёрных сводах.",
            "exits": [
                {"id": "e_mine_iron", "label": "наверх, в железные штольни", "to": "l_mine_iron"},
                {"id": "e_mine_deep", "label": "спуск в платиновые залы", "to": "l_mine_platinum"},
            ],
            "entities": [
                vein_node("ore_silver_1", "Серебряная жила", "ore_silver", 3, vein_respawn(100), 100, 3, silver_mixed, silver_pure, silver_chance),
                vein_node("ore_silver_2", "Серебряная жила", "ore_silver", 3, vein_respawn(100), 100, 3, silver_mixed, silver_pure, silver_chance),
                vein_node("ore_gold_1", "Золотая жила", "ore_gold", 2, vein_respawn(126), 126, 3, gold_mixed, gold_pure, gold_chance),
                vein_node("ore_gold_2", "Золотая жила", "ore_gold", 2, vein_respawn(126), 126, 3, gold_mixed, gold_pure, gold_chance),
            ],
        },
        {
            "id": "l_mine_platinum",
            "name": "Платиновые залы",
            "spawn": False,
            "flags": ["pvp"],
            "text": "Тяжёлый платиновый блеск; адамантит впитывает свет факелов.",
            "exits": [
                {"id": "e_mine_silver", "label": "наверх, в серебряные копи", "to": "l_mine_silver"},
                {"id": "e_mine_deep", "label": "спуск в глубины мифрила", "to": "l_mine_mithril"},
            ],
            "entities": [
                vein_node("ore_platinum_1", "Платиновая жила", "ore_platinum", 2, vein_respawn(150), 150, 4, platinum_mixed, platinum_pure, platinum_chance),
                vein_node("ore_platinum_2", "Платиновая жила", "ore_platinum", 2, vein_respawn(150), 150, 4, platinum_mixed, platinum_pure, platinum_chance),
                vein_node("ore_adamantite_1", "Адамантитовая жила", "ore_adamantite", 2, vein_respawn(176), 176, 4, adamantite_mixed, adamantite_pure, adamantite_chance),
                vein_node("ore_adamantite_2", "Адамантитовая жила", "ore_adamantite", 2, vein_respawn(176), 176, 4, adamantite_mixed, adamantite_pure, adamantite_chance),
            ],
        },
        {
            "id": "l_mine_mithril",
            "name": "Глубины мифрила",
            "spawn": False,
            "flags": ["pvp"],
            "text": "Холодные залы; мифрил светится голубым в трещинах чёрного камня.",
            "exits": [
                {"id": "e_mine_platinum", "label": "наверх, в платиновые залы", "to": "l_mine_platinum"},
            ],
            "entities": [
                vein_node("ore_mithril_1", "Мифриловая жила", "ore_mithril", 2, vein_respawn(200), 200, 5, mithril_mixed, mithril_pure, mithril_chance),
                vein_node("ore_mithril_2", "Мифриловая жила", "ore_mithril", 2, vein_respawn(200), 200, 5, mithril_mixed, mithril_pure, mithril_chance),
            ],
        },
    ]

def update_locations(locations: dict) -> None:
    locs = locations["locations"]
    by_id = {loc["id"]: loc for loc in locs}
    generated = {loc["id"] for loc in mine_locations()}

    # Удаляем прошлые сгенерированные рудники.
    locs[:] = [loc for loc in locs if loc["id"] not in generated]
    locs.extend(mine_locations())
    by_id = {loc["id"]: loc for loc in locs}

    village = by_id["l_village"]
    village["exits"] = [e for e in village["exits"] if e["id"] != "e_mine_copper"]
    village["exits"].append(
        {
            "id": "e_mine_copper",
            "label": "тропа в медный карьер",
            "to": "l_mine_copper",
        }
    )
    if not any(e.get("id") == "st_jeweler" for e in village["entities"]):
        village["entities"].append(
            {
                "id": "st_jeweler",
                "kind": "object",
                "name": "Ювелирный стол",
                "station": "jeweler_table",
            }
        )

    forest = by_id["l_forest"]
    copper_node = next(
        (e for e in forest["entities"] if e.get("id") == "r_forest_ore"), None
    )
    if copper_node is not None:
        copper_node["required_level"] = 1
        # Стартовая жила — без камней, чтобы базовый сбор оставался простым.
        copper_node.pop("gems", None)

    # Оловянная жила в старом лесу — доступна с 35 уровня.
    forest["entities"] = [e for e in forest["entities"] if e.get("id") != "r_forest_tin"]
    forest["entities"].append(
        vein_node(
            "r_forest_tin",
            "Оловянная жила",
            "ore_tin",
            2,
            vein_respawn(35),
            35,
            1,
            TIN_MIXED,
            TIN_PURE,
            0.06,
        )
    )
    del by_id  # больше не нужен


def update_items(items: dict) -> None:
    generated = generated_items()
    ids = {item["id"] for item in generated}
    items["items"][:] = [item for item in items["items"] if item["id"] not in ids]
    items["items"].extend(generated)


def update_recipes(recipes: dict) -> None:
    generated = generated_recipes()
    recipes["recipes"][:] = [
        r for r in recipes["recipes"] if not r["id"].startswith("rec_cut_")
    ]
    recipes["recipes"].extend(generated)


def update_shops(shops: dict) -> None:
    for npc in shops["npcs"]:
        if npc.get("id") != "npc_city_elder":
            continue
        for currency in npc.get("currencies", []):
            if currency.get("currency") != "gold_coin":
                continue
            items = currency.setdefault("items", [])
            items[:] = [i for i in items if i != "chisel"]
            if "tool_chisel" not in items:
                items.append("tool_chisel")


def main() -> None:
    with open(os.path.join(CONTENT, "items.json"), encoding="utf-8") as fh:
        items = json.load(fh)
    with open(os.path.join(CONTENT, "locations.json"), encoding="utf-8") as fh:
        locations = json.load(fh)
    with open(os.path.join(CONTENT, "recipes.json"), encoding="utf-8") as fh:
        recipes = json.load(fh)
    with open(os.path.join(CONTENT, "shops.json"), encoding="utf-8") as fh:
        shops = json.load(fh)

    update_items(items)
    update_locations(locations)
    update_recipes(recipes)
    update_shops(shops)

    for name, data in [
        ("items.json", items),
        ("locations.json", locations),
        ("recipes.json", recipes),
        ("shops.json", shops),
    ]:
        with open(os.path.join(CONTENT, name), "w", encoding="utf-8") as fh:
            fh.write(json.dumps(data, ensure_ascii=False, indent=2) + "\n")

    print(
        f"готово: предметов {len(items['items'])}, рецептов {len(recipes['recipes'])}, "
        f"локаций {len(locations['locations'])}"
    )


if __name__ == "__main__":
    main()
