//! Присутствие игроков в локации: строки `kind: player` в снапшоте §6 и
//! адресные `evt/entity/spawn`/`evt/entity/despawn` остальным в локации
//! (вход в игру, переход, выход из мира/дисконнект).
//!
//! Снапшот строит `World::build_location` (там же фильтр по `char_loc`);
//! здесь — проводной вид сущности и рассылка событий по реестру мира.

use ld2_protocol::names;

use ld2_protocol::evt::{EntityDespawn, EntitySpawn};
use ld2_protocol::types::{Entity, EntityKind, Hp};

use crate::state::SharedState;

use super::event_text;
use crate::consts::GENDER_FEMALE;

/// Сущность игрока для `location.entities` (§6: `kind = player`).
/// `hp` не передаём: мир не хранит HP персонажей (истина — БД), а
/// устаревшая шкала хуже её отсутствия.
pub(crate) fn player_entity(char_id: &str, name: &str) -> Entity {
    Entity {
        id: char_id.to_string(),
        kind: EntityKind::Player,
        name: name.to_string(),
        hp: None::<Hp>,
        tags: None,
        was_player: None,
        aggro: None,
        boss: None,
        attacking: None,
        next_attack_at: None,
        attack_interval: None,
        extra: Default::default(),
    }
}

/// Отправить событие всем в локации, кроме самого действующего лица.
fn broadcast(state: &SharedState, location_id: &str, except: &str, text: String) {
    if text.is_empty() {
        return;
    }
    let world = state.world.lock().expect("мир");
    for addr in world.chars_in_location(location_id) {
        if addr != except {
            world.send_to_char(&addr, text.clone());
        }
    }
}

/// Глагол прихода (M29, заказчик): «пришел/пришла» по роду — формат
/// `[действие] [имя]` (в отличие от ухода/гибели, где имя впереди).
pub(crate) fn came_line(name: &str, gender: &str) -> String {
    let verb = if gender == GENDER_FEMALE {
        "пришла"
    } else {
        "пришел"
    };
    format!("{verb} {name}")
}

/// Глагол гибели (M29, заказчик): «погиб»/«погибла» по роду.
pub(crate) fn died_line(name: &str, gender: &str) -> String {
    let verb = if gender == GENDER_FEMALE {
        "погибла"
    } else {
        "погиб"
    };
    format!("{name} {verb}")
}

/// Кто-то пришел в локацию (переход): `evt/entity/spawn` остальным
/// с готовой строкой журнала; вход в мир/респавн — `text: None`
/// (клиентский шаблон «{name} появляется»).
pub(crate) fn announce_spawn(
    state: &SharedState,
    location_id: &str,
    entity: Entity,
    text: Option<String>,
) {
    let except = entity.id.clone();
    let payload = EntitySpawn {
        entity,
        location_id: location_id.to_string(),
        text,
    };
    let text = event_text(
        names::EVT_ENTITY_SPAWN,
        None,
        serde_json::to_value(payload).unwrap_or_default(),
    );
    broadcast(state, location_id, &except, text);
}

/// Игрок ушёл из локации: `evt/entity/despawn` остальным.
/// `text` — готовая строка журнала соседям (M29): «{имя} ушел — {выход}»
/// при переходе, без него — стандартное «{name} исчезает».
pub(crate) fn announce_despawn(
    state: &SharedState,
    location_id: &str,
    char_id: &str,
    text: Option<String>,
) {
    let payload = EntityDespawn {
        entity_id: char_id.to_string(),
        location_id: location_id.to_string(),
        kind: Some(EntityKind::Player),
        picked_by: None,
        picked_gender: None,
        text,
    };
    let wire = event_text(
        names::EVT_ENTITY_DESPAWN,
        None,
        serde_json::to_value(payload).unwrap_or_default(),
    );
    broadcast(state, location_id, char_id, wire);
}

/// Имя привязанного персонажа из реестра мира (для событий смерти/перехода).
pub(crate) fn char_name(state: &SharedState, char_id: &str) -> Option<String> {
    let world = state.world.lock().expect("мир");
    world.char_name(char_id)
}
