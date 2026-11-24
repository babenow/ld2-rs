//! Типы NPC-диалога и их методы (`NpcNode`, `NpcDialogView`, `Dialog`
//! и вспомогательные) — вынесены из `store.rs` (RF4b). Поведение не
//! менялось: только перенос.

use super::*;

/// M18/F37/F42: узел диалога — строка NPC + её видимые ответы (тексты уже
/// отрендерены сервером, `{name}` подставлен).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcNode {
    pub id: String,
    pub text: String,
    /// Квест, который предлагает узел (секция «Квесты» окна).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quest: Option<String>,
    #[serde(default)]
    pub replies: Vec<NpcReplyOption>,
}

impl NpcNode {
    /// M18/F42: абсолютные дедлайны кулдауна реплик — по ним UI ведёт
    /// живой отсчёт `(мм:сс)` (ADR-008). Вызывается при разборе payload.
    pub fn stamp_wait_deadlines(&mut self) {
        let now = local_epoch_ms();
        for reply in &mut self.replies {
            if let Some(wait_s) = reply.wait_s {
                reply.wait_until_ms = Some(now + (wait_s as i64) * 1000);
            }
        }
    }
}

/// M18/F37/F42: ответ персонажа, видимый в текущем узле (`id` — ключ
/// `cmd/npc.reply`).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcReplyOption {
    pub id: String,
    pub text: String,
    /// M18/F42: секунды до конца кулдауна целевой строки — ответ
    /// показывается неактивным с таймером (ADR-008); `None` — активен.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_s: Option<u64>,
    /// M18/F42: абсолютный дедлайн кулдауна (epoch-мс) — ставится при
    /// разборе payload, живой отсчёт таймера в UI (wire не несёт).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_until_ms: Option<i64>,
    /// M18/F43: действие ответа (`"shop"`) — клиент открывает магазин
    /// вместо перехода по ветке; `None` — обычный ответ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// M18/F37: строка транскрипта окна диалога — лента NPC ↔ персонаж.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcTranscriptLine {
    /// `true` — реплика персонажа (ответ).
    pub me: bool,
    pub text: String,
}

/// M18/F39: товар магазина в цене активной валютной вкладки.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcShopItem {
    pub def_id: String,
    pub name: String,
    #[serde(default)]
    pub rarity: String,
    /// Цена за партию в единицах валюты вкладки.
    pub price: i64,
    /// Размер партии (qty из контента, ≥1).
    #[serde(default = "shop_item_qty")]
    pub qty: i64,
    /// M18/F40: полный снапшот — карточка информации по клику строки.
    #[serde(default)]
    pub item: Option<ld2_protocol::types::ItemSnapshot>,
}

fn shop_item_qty() -> i64 {
    1
}

/// M18/F39: валютная вкладка магазина.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcShopTab {
    /// `def_id` валюты (для баланса и `cmd/shop.buy`).
    pub currency: String,
    pub currency_name: String,
    #[serde(default)]
    pub items: Vec<NpcShopItem>,
}

/// M18/F39: открытый магазин NPC (внутри окна диалога — очищается
/// вместе с `npc_dialog` на всех путях закрытия).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcShopView {
    #[serde(default)]
    pub tabs: Vec<NpcShopTab>,
}

/// M18/F35/F37/F42: открытое окно диалога NPC — payload ack
/// `cmd/npc.talk` `{dialog: {id, name}, node, quests, actions}`
/// (`greeting` убран — ADR-008); транскрипт накапливает ленту
/// (узлы ↔ ответы персонажа).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcDialogView {
    pub id: String,
    pub name: String,
    /// Текущий узел; `None` — диалог не начат/закончен.
    #[serde(default)]
    pub node: Option<NpcNode>,
    #[serde(default)]
    pub transcript: Vec<NpcTranscriptLine>,
    #[serde(default)]
    pub quests: Vec<ld2_protocol::types::QuestState>,
    /// M18/F38: контекстные кнопки статус-бара от сервера — только
    /// реально существующие системы (`quests`/`train`/`shop`).
    #[serde(default)]
    pub actions: Vec<String>,
    /// M18/F39: открытый магазин (по кнопке — `cmd/shop.list`).
    #[serde(default)]
    pub shop: Option<NpcShopView>,
    /// M18/F39: активная валютная вкладка магазина.
    #[serde(default)]
    pub shop_currency: Option<String>,
    /// M18/F44: обучение у этого NPC (ADR-009) — навыки с ценой.
    #[serde(default)]
    pub training: Vec<NpcTraining>,
    /// M21: профессии этого NPC с ценой (изучение — craft.learn).
    #[serde(default)]
    pub professions: Vec<NpcProfession>,
    /// M18/F44: открыта ли панель обучения (реплика `action=train`,
    /// паритет магазина); чистится вместе с окном диалога.
    #[serde(default)]
    pub training_open: bool,
    /// M22: открыта ли ИСТОРИЯ диалога (полная лента по кнопке в
    /// статусбаре окна); в самом диалоге — только текущая реплика.
    #[serde(default)]
    pub history_open: bool,
}

/// M18/F44: обучение навыку у NPC в диалоге (ADR-009) — повышение ранга
/// стоит очков опыта (пул) + платы из контента.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcTraining {
    pub skill: String,
    /// Плата очками опыта (пул квестов).
    pub points: i64,
    /// Плата золотом (монеты); 0 — без золота.
    #[serde(default)]
    pub gold: i64,
    /// Плата предметами.
    #[serde(default)]
    pub items: Vec<NpcTrainingItem>,
}

/// M21: профессия, которой обучает NPC в диалоге — цена золотом
/// (`price_profession`); изучение — `cmd/craft.learn`.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcProfession {
    pub id: String,
    /// Цена обучения золотом; старый сервер без поля →0.
    #[serde(default)]
    pub gold: i64,
}

/// M18/F44: предмет платы за обучение.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NpcTrainingItem {
    pub def_id: String,
    #[serde(default = "shop_item_qty")]
    pub qty: i64,
}

impl NpcDialogView {
    /// Разбор payload ack (`None` — нет `dialog`; лента — корневой узел,
    /// `quests`/`actions` без поля → пусто; `greeting` больше нет —
    /// ADR-008).
    pub fn from_payload(payload: &serde_json::Value) -> Option<Self> {
        let dialog = payload.get("dialog")?;
        let mut node = match payload.get("node") {
            Some(value) if !value.is_null() => {
                serde_json::from_value::<NpcNode>(value.clone()).ok()
            }
            _ => None,
        };
        // M18/F42: дедлайны для живого отсчёта таймеров реплик.
        node.as_mut().map(NpcNode::stamp_wait_deadlines);
        let mut transcript = Vec::new();
        if let Some(node) = &node {
            transcript.push(NpcTranscriptLine {
                me: false,
                text: node.text.clone(),
            });
        }
        Some(Self {
            id: dialog["id"].as_str()?.into(),
            name: dialog["name"].as_str()?.into(),
            node,
            transcript,
            quests: serde_json::from_value(payload.get("quests")?.clone()).unwrap_or_default(),
            actions: serde_json::from_value(payload.get("actions").cloned().unwrap_or_default())
                .unwrap_or_default(),
            shop: None,
            shop_currency: None,
            training: serde_json::from_value(payload.get("training").cloned().unwrap_or_default())
                .unwrap_or_default(),
            professions: serde_json::from_value(
                payload.get("professions").cloned().unwrap_or_default(),
            )
            .unwrap_or_default(),
            // M18/F44: панель обучения закрыта — открывает реплика.
            training_open: false,
            // M22: история закрыта — открывает кнопка в статусбаре.
            history_open: false,
        })
    }
}

/// M11: какой всплывающий диалог активен. Приоритет и гейт экрана — здесь
/// же их читает UI-диспетчер gpui-клиента (E2E-тест вехи M11.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialog {
    /// ФР-06: смерть — закрывается только серверным подтверждением.
    Death,
    Examine,
    Loot,
    PartyInvite,
    GuildInvite,
    Trade,
    /// M17/F30a: выбор оружия под инкрустацию руны/самоцвета.
    Inscribe,
    Create,
    Delete,
}
