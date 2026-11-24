//! Применение патчей предметов и снапшота — вынесено из store.rs (RF4i).

use ld2_protocol::evt::{PatchOp, PatchOpKind};

/// Применение `payload` ack'а `socket.*`: `{staff, rune?}` → инвентарь.
pub(crate) fn apply_item_patch(
    ch: &mut ld2_protocol::types::CharacterState,
    payload: &serde_json::Value,
) {
    if let Some(staff) = payload.get("staff").filter(|v| !v.is_null()) {
        if let Ok(staff) =
            serde_json::from_value::<ld2_protocol::types::ItemSnapshot>(staff.clone())
        {
            if let Some(slot) = ch.inventory.iter_mut().find(|i| i.uid == staff.uid) {
                *slot = staff;
            }
        }
    }
    if let Some(rune) = payload.get("rune").filter(|v| !v.is_null()) {
        if let Ok(rune) = serde_json::from_value::<ld2_protocol::types::ItemSnapshot>(rune.clone())
        {
            // M18 (указание заказчика): извлечённый самоцвет может
            // попасть в существующий стак — запись уже в инвентаре:
            // обновляем (qty выросло), иначе — добавляем.
            if let Some(slot) = ch.inventory.iter_mut().find(|i| i.uid == rune.uid) {
                *slot = rune;
            } else {
                ch.inventory.push(rune);
            }
        }
    }
}

/// R11 (§3): применение JSON-patch к снапшоту. Путь — RFC 6901
/// pointer («/location/entities/0/hp/cur»); `~0`/`~1` — эскейпы.
/// `false` — путь не существует / битое значение (клиент запрашивает
/// полный снапшот вместо тихого рассинхрона).
pub(crate) fn apply_json_patch(value: &mut serde_json::Value, patch: &[PatchOp]) -> bool {
    patch.iter().all(|op| apply_patch_op(value, op))
}

pub(crate) fn apply_patch_op(value: &mut serde_json::Value, op: &PatchOp) -> bool {
    let segments: Vec<String> = op
        .path
        .split('/')
        .skip(1)
        .map(|s| s.replace("~1", "/").replace("~0", "~"))
        .collect();
    if segments.is_empty() {
        return false;
    }
    let mut cur = value;
    for seg in &segments[..segments.len() - 1] {
        let next = match cur {
            serde_json::Value::Object(map) => map.get_mut(seg),
            serde_json::Value::Array(arr) => {
                seg.parse::<usize>().ok().and_then(move |i| arr.get_mut(i))
            }
            _ => None,
        };
        let Some(next) = next else {
            return false;
        };
        cur = next;
    }
    let Some(last) = segments.last() else {
        return false;
    };
    match op.op {
        PatchOpKind::Remove => match cur {
            serde_json::Value::Object(map) => map.remove(last).is_some(),
            serde_json::Value::Array(arr) => last
                .parse::<usize>()
                .ok()
                .filter(|i| *i < arr.len())
                .map(|i| arr.remove(i))
                .is_some(),
            _ => false,
        },
        PatchOpKind::Add | PatchOpKind::Replace => {
            let Some(val) = op.value.clone() else {
                return false;
            };
            match cur {
                serde_json::Value::Object(map) => {
                    // replace — только существующий ключ (иначе клиент
                    // промолчал бы о структуре, которой не знал).
                    if op.op == PatchOpKind::Replace && !map.contains_key(last) {
                        return false;
                    }
                    map.insert(last.clone(), val);
                    true
                }
                serde_json::Value::Array(arr) => match last.parse::<usize>() {
                    Ok(i) if i < arr.len() => {
                        arr[i] = val;
                        true
                    }
                    Ok(i) if i == arr.len() && op.op == PatchOpKind::Add => {
                        arr.push(val);
                        true
                    }
                    _ => false,
                },
                _ => false,
            }
        }
    }
}
