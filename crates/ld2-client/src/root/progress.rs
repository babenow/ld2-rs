//! Прогресс-бары, таймеры кулдаунов и индикаторы баров (атака, руны)
//! игрового экрана. Вынесено из `root.rs` (RF3i).

use super::*;

impl Root {
    /// Текущее время (epoch, **мс**) с поправкой на часы сервера —
    /// база для отсчёта кулдаунов (M14/F4).
    pub(super) fn now_ms(&self) -> i64 {
        local_epoch_ms() + self.store.time_offset * 1000
    }

    /// M14/F4: остаток собственного кулдауна действия, мс. Сервер
    /// шлёт `until` в секундах → граница секунды в мс, чтобы отсчёт
    /// на кнопке шёл плавно (тик 50 мс), а не дёргался раз в секунду.
    pub(super) fn own_cd_left_ms(&self, skill: &str, now_ms: i64) -> i64 {
        let until_s = self.store.combat_cooldowns.get(skill).copied().unwrap_or(0);
        until_s.saturating_mul(1000) - now_ms
    }

    /// Навигация по направлению (ФР-03; хоткеи из config — ФР-19).
    pub(super) fn nav_direction(&mut self, dir: &str) {
        let Some(exit) = self.store.snapshot.as_ref().and_then(|s| {
            s.location
                .exits
                .iter()
                .find(|e| e.dir.as_deref() == Some(dir))
                .map(|e| e.id.clone())
        }) else {
            return;
        };
        let cmd = ld2_protocol::cmd::LocationMove { exit_id: exit };
        if self.send_or_notice(&cmd, SendSlot::World) {
            self.store.on_world_sent(Act::Move)
        }
    }

    /// Цикл целей по Tab (ФР-05).
    pub(super) fn cycle_target(&mut self) {
        self.store.cycle_target();
    }

    /// M24: запуск единого прогресс-бара левой колонки — подпись
    /// «Действие · имя» и длительность действия; гаснет в `tick` по
    /// исчезновению [`ProgressSource`]-источника.
    pub(super) fn start_progress(
        &mut self,
        source: ProgressSource,
        label: String,
        duration_ms: i64,
    ) {
        self.cast_progress = Some(CastProgress {
            source,
            label,
            started_ms: self.now_ms(),
            duration_ms: duration_ms.max(1),
        });
    }

    /// M17/F30a: начало каста инкрустации — окно закрывается, полоса
    /// идёт 3 с без отмены; повторный каст во время активного не стартует.
    pub(super) fn start_inscribe(&mut self, rune_uid: String, target_uid: String) {
        if self.cast_inscribe.is_some() {
            return;
        }
        self.store.close_inscribe();
        // M24: единый прогресс-бар — подпись «Действие · имя».
        let target = self
            .store
            .snapshot
            .as_ref()
            .and_then(|s| s.char.inventory.iter().find(|i| i.uid == target_uid))
            .map(|i| i.name.clone())
            .unwrap_or_default();
        self.cast_inscribe = Some((rune_uid, target_uid, self.now_ms() + INSCRIBE_MS));
        self.start_progress(
            ProgressSource::Inscribe,
            format!("{} · {}", t(self.lang, Key::InscribeCasting), target),
            INSCRIBE_MS,
        );
    }

    /// M17/F30a: каст завершён — cmd/socket.insert в первый свободный
    /// сокет (индекс по свежему снапшоту; сервер валидирует — занятый
    /// слот/нет сокетов → тост через notice).
    pub(super) fn finish_inscribe(&mut self, rune_uid: String, target_uid: String) {
        // M18/F58f (баг «не работает инкрустация через меню камня»):
        // слот — первый свободный, **принимающий тип вставки**
        // (accepts, ADR-012): иначе самоцвет шёл в рунный слот и
        // сервер отвечал «слот принимает только руны».
        let snapshot = self.store.snapshot.clone();
        let insert_type = snapshot
            .as_ref()
            .and_then(|s| s.char.inventory.iter().find(|i| i.uid == rune_uid))
            .map(|i| i.item_type.clone());
        let index = snapshot
            .as_ref()
            .and_then(|s| s.char.inventory.iter().find(|i| i.uid == target_uid))
            .and_then(|item| item.sockets.as_ref())
            .and_then(|sockets| {
                insert_type
                    .as_deref()
                    .and_then(|t| sheet::first_compatible_socket(sockets, t))
            });
        let Some(index) = index else {
            self.store.notice = Some(t(self.lang, Key::InscribeNoWeapons).to_string());
            return;
        };
        let cmd = ld2_protocol::cmd::SocketInsert {
            staff_uid: target_uid,
            socket_index: index as u32,
            rune_uid,
        };
        self.send_cmd(&cmd, Act::Socket);
    }

    /// M25: есть ли в локации монстры с непрожёванным баром атаки —
    /// тик50 мс, чтобы заливка шла плавно.
    pub(super) fn attack_bars_active(&self) -> bool {
        let Some(snap) = self.store.snapshot.as_ref() else {
            return false;
        };
        let now = self.now_ms();
        snap.location.entities.iter().any(|e| {
            e.kind == ld2_protocol::types::EntityKind::Monster
                && e.next_attack_at.unwrap_or(0) > now
                && e.attack_interval.unwrap_or(0) > 0
        })
    }

    /// M24: заряды рун в надетом снаряжении ниже максимума (режим
    /// `time`) — тик 50 мс, пока прогрессбары заполняются.
    pub(super) fn rune_bar_active(&self) -> bool {
        let Some(snap) = self.store.snapshot.as_ref() else {
            return false;
        };
        let Some(session) = self.store.session.as_ref() else {
            return false;
        };
        if session.magic.rune_recharge != "time" {
            return false;
        }
        let equipped: std::collections::HashSet<&str> = snap
            .char
            .equip
            .values()
            .flatten()
            .map(String::as_str)
            .collect();
        snap.char
            .inventory
            .iter()
            .filter(|i| equipped.contains(i.uid.as_str()))
            .flat_map(|i| i.sockets.iter().flatten())
            .any(|s| s.rune.as_ref().is_some_and(|r| r.charges < r.max))
    }

    /// M24: активны ли бафы рун — тик для отсчёта остатка и
    /// моргания на последних3 секундах.
    pub(super) fn rune_buffs_active(&self) -> bool {
        let Some(snap) = self.store.snapshot.as_ref() else {
            return false;
        };
        let now = self.now_ms() / 1000;
        filter_active_buffs(&snap.char.buffs, now)
            .iter()
            .any(|b| b.kind.starts_with("rune_"))
    }
}
