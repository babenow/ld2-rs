//! RF1v: админ-панель и dev-команды (M24) — выдача/удаление предметов,
//! монстров, контейнеров и товаров магазина, god mode, очистка, reload
//! контента и ответы диалога.
//!
//! Методы `impl Session` разложены по подмодулям: `target`, `give`,
//! `content`; поля `Session` по-прежнему приватны для модуля сессии.

mod content;
mod give;
mod target;

use ld2_protocol::cmd::{
    AdminClear, AdminContentReload, AdminGiveAsk, AdminGodMode, AdminItems, AdminRemoveAsk,
    AdminTargetInfo,
};
use ld2_protocol::envelope::Envelope;
use ld2_protocol::evt::{ItemAdded, ItemRemovalReason, ItemRemoved, ItemState};
use ld2_protocol::types::Entity;
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::rules::{entity_loot_defs, item_snapshot};
use super::{event_text, out_event, short_version, Out, Session};
use crate::db;
