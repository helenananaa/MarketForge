use super::pending_closes::PendingCloseBook;
use super::pending_entries::PendingEntryBook;
use super::pending_exits::PendingExitBook;
use super::types::{InternalOrderKey, OcaGroupKey, OcaMember, OcaPeerEffects, OcaType};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub(super) struct OrderBook {
    entries: PendingEntryBook,
    exits: PendingExitBook,
    closes: PendingCloseBook,
    oca_membership: HashMap<OcaMember, OcaGroupKey>,
    next_order_sequence: u64,
}

impl OrderBook {
    pub(super) fn new() -> Self {
        Self {
            entries: PendingEntryBook::new(),
            exits: PendingExitBook::new(),
            closes: PendingCloseBook::new(),
            oca_membership: HashMap::new(),
            next_order_sequence: 0,
        }
    }

    pub(super) fn allocate_key(&mut self) -> InternalOrderKey {
        let key = InternalOrderKey(self.next_order_sequence);
        self.next_order_sequence = self.next_order_sequence.wrapping_add(1);
        key
    }

    #[cfg(test)]
    pub(super) fn next_order_sequence(&self) -> u64 {
        self.next_order_sequence
    }

    pub(super) fn with_entry_allocator<R>(
        &mut self,
        f: impl FnOnce(&mut PendingEntryBook, &mut dyn FnMut() -> InternalOrderKey) -> R,
    ) -> R {
        let OrderBook {
            entries,
            next_order_sequence,
            ..
        } = self;
        let mut allocate = || {
            let key = InternalOrderKey(*next_order_sequence);
            *next_order_sequence = next_order_sequence.wrapping_add(1);
            key
        };
        f(entries, &mut allocate)
    }

    pub(super) fn with_close_allocator<R>(
        &mut self,
        f: impl FnOnce(&mut PendingCloseBook, &mut dyn FnMut() -> InternalOrderKey) -> R,
    ) -> R {
        let OrderBook {
            closes,
            next_order_sequence,
            ..
        } = self;
        let mut allocate = || {
            let key = InternalOrderKey(*next_order_sequence);
            *next_order_sequence = next_order_sequence.wrapping_add(1);
            key
        };
        f(closes, &mut allocate)
    }

    pub(super) fn assign_exit_key(&mut self, pending_exit: &mut super::pending_exits::PendingExit) {
        if let Some(existing) = self.exits.find_by_identity_and_key(
            &pending_exit.id,
            &pending_exit.from_entry,
            pending_exit.target_trade_key,
        ) {
            pending_exit.key = existing.key;
        } else {
            pending_exit.key = self.allocate_key();
        }
    }

    pub(super) fn replace_or_append_exit(
        &mut self,
        mut pending_exit: super::pending_exits::PendingExit,
    ) {
        self.assign_exit_key(&mut pending_exit);
        self.exits.replace_or_append(pending_exit);
    }

    pub(super) fn replace_all_exit(&mut self, mut pending_exit: super::pending_exits::PendingExit) {
        self.assign_exit_key(&mut pending_exit);
        if pending_exit.from_entry.is_empty() {
            self.exits.replace_all(pending_exit);
        } else {
            self.exits.replace_for_entry(pending_exit);
        }
    }

    pub(super) fn replace_all_exits(
        &mut self,
        mut pending_exits: Vec<super::pending_exits::PendingExit>,
    ) {
        for pending_exit in &mut pending_exits {
            self.assign_exit_key(pending_exit);
        }
        self.exits.replace_all_many(pending_exits);
    }

    pub(super) fn entries(&self) -> &PendingEntryBook {
        &self.entries
    }

    pub(super) fn entries_mut(&mut self) -> &mut PendingEntryBook {
        &mut self.entries
    }

    pub(super) fn exits(&self) -> &PendingExitBook {
        &self.exits
    }

    pub(super) fn exits_mut(&mut self) -> &mut PendingExitBook {
        &mut self.exits
    }

    #[allow(dead_code)]
    pub(super) fn closes(&self) -> &PendingCloseBook {
        &self.closes
    }

    pub(super) fn closes_mut(&mut self) -> &mut PendingCloseBook {
        &mut self.closes
    }

    pub(super) fn cancel_id(&mut self, id: &str) {
        let members = self.pending_members_for_public_id(id);
        self.entries.cancel_id(id);
        self.exits.cancel_id(id);
        self.closes.cancel_id(id);
        for member in members {
            self.oca_membership.remove(&member);
        }
        self.prune_oca_membership();
    }

    pub(super) fn clear_all(&mut self) {
        self.entries.clear_all();
        self.exits.clear_all();
        self.closes.clear_all();
        self.oca_membership.clear();
    }

    pub(super) fn clear_exits_for_entry(&mut self, entry_id: &str) {
        self.exits.clear_for_entry(entry_id);
        self.prune_oca_membership();
    }

    pub(super) fn retain_exits_for_pending_entries(&mut self, filling_entry_id: Option<&str>) {
        let mut pending_ids: HashSet<String> =
            self.entries.iter().map(|entry| entry.id.clone()).collect();
        if let Some(id) = filling_entry_id {
            pending_ids.insert(id.to_owned());
        }
        self.exits.retain_for_pending_entries(&pending_ids);
        self.prune_oca_membership();
    }

    #[allow(dead_code)]
    pub(super) fn assign_oca(&mut self, member: OcaMember, group: OcaGroupKey) {
        self.oca_membership.insert(member, group);
    }

    #[allow(dead_code)]
    pub(super) fn oca_group(&self, member: &OcaMember) -> Option<&OcaGroupKey> {
        self.oca_membership.get(member)
    }

    #[allow(dead_code)]
    pub(super) fn oca_members_in_group(&self, group: &OcaGroupKey) -> Vec<OcaMember> {
        let mut members = self
            .oca_membership
            .iter()
            .filter(|(_, assigned)| *assigned == group)
            .map(|(member, _)| member.clone())
            .collect::<Vec<_>>();
        members.sort_by_key(|member| match member {
            OcaMember::Order(key) => (0, key.0),
            OcaMember::Exit {
                target_trade_key, ..
            } => (1, target_trade_key.unwrap_or(u64::MAX)),
        });
        members
    }

    pub(super) fn apply_oca_after_exit_fill(
        &mut self,
        id: &str,
        from_entry: &str,
        target_trade_key: Option<u64>,
        filled_qty: f64,
    ) {
        let filled = OcaMember::Exit {
            id: id.to_owned(),
            from_entry: from_entry.to_owned(),
            target_trade_key,
        };
        let Some(group) = self.oca_membership.get(&filled).cloned() else {
            return;
        };
        self.oca_membership.remove(&filled);
        if group.oca_type != OcaType::Reduce {
            return;
        }
        let _ = self.apply_group_peer_effects(&filled, &group, Some(filled_qty));
    }

    pub(super) fn apply_oca_after_fill(
        &mut self,
        filled_key: InternalOrderKey,
        filled_qty: f64,
    ) -> OcaPeerEffects {
        let filled = OcaMember::Order(filled_key);
        let Some(group) = self.oca_membership.get(&filled).cloned() else {
            return OcaPeerEffects::default();
        };
        self.oca_membership.remove(&filled);
        match group.oca_type {
            OcaType::None => OcaPeerEffects::default(),
            OcaType::Cancel => self.apply_group_peer_effects(&filled, &group, None),
            OcaType::Reduce => self.apply_group_peer_effects(&filled, &group, Some(filled_qty)),
        }
    }

    fn apply_group_peer_effects(
        &mut self,
        filled: &OcaMember,
        group: &OcaGroupKey,
        reduce_by: Option<f64>,
    ) -> OcaPeerEffects {
        let peers = self.oca_members_in_group(group);
        let mut effects = OcaPeerEffects::default();
        for peer in peers {
            if &peer == filled {
                continue;
            }
            match peer {
                OcaMember::Order(key) => match reduce_by {
                    None => {
                        self.entries.remove_by_key(key);
                        self.oca_membership.remove(&OcaMember::Order(key));
                        effects.cancelled.push(key);
                    }
                    Some(filled_qty) => {
                        if let Some(pending) = self.entries.find_mut_by_key(key) {
                            let remaining = (pending.quantity - filled_qty).max(0.0);
                            if remaining <= 0.0 {
                                self.entries.remove_by_key(key);
                                self.oca_membership.remove(&OcaMember::Order(key));
                                effects.cancelled.push(key);
                                effects.reduced.insert(key, 0.0);
                            } else {
                                pending.quantity = remaining;
                                effects.reduced.insert(key, remaining);
                            }
                        } else {
                            effects.reduce_taken.push(key);
                        }
                    }
                },
                OcaMember::Exit {
                    id: peer_id,
                    from_entry: peer_from,
                    target_trade_key: peer_key,
                } => {
                    let member = OcaMember::Exit {
                        id: peer_id.clone(),
                        from_entry: peer_from.clone(),
                        target_trade_key: peer_key,
                    };
                    match reduce_by {
                        None => {
                            self.exits
                                .remove_by_identity_and_key(&peer_id, &peer_from, peer_key);
                            self.oca_membership.remove(&member);
                        }
                        Some(filled_qty) => {
                            if let Some(pending) = self
                                .exits
                                .find_mut_by_identity_and_key(&peer_id, &peer_from, peer_key)
                            {
                                let remaining = (pending.reserved_quantity - filled_qty).max(0.0);
                                if remaining <= 0.0 {
                                    self.exits
                                        .remove_by_identity_and_key(&peer_id, &peer_from, peer_key);
                                    self.oca_membership.remove(&member);
                                } else {
                                    pending.reserved_quantity = remaining;
                                }
                            } else {
                                self.oca_membership.remove(&member);
                            }
                        }
                    }
                }
            }
        }
        effects
    }

    pub(super) fn clear_oca_order(&mut self, key: InternalOrderKey) {
        self.oca_membership.remove(&OcaMember::Order(key));
    }

    fn prune_oca_membership(&mut self) {
        let live_order_keys = self
            .entries
            .iter()
            .map(|pending| pending.key)
            .chain(self.closes.iter().map(|pending| pending.key))
            .collect::<HashSet<_>>();
        self.oca_membership.retain(|member, _| match member {
            OcaMember::Order(key) => live_order_keys.contains(key),
            OcaMember::Exit {
                id,
                from_entry,
                target_trade_key,
            } => self
                .exits
                .find_by_identity_and_key(id, from_entry, *target_trade_key)
                .is_some(),
        });
    }

    fn pending_members_for_public_id(&self, id: &str) -> Vec<OcaMember> {
        let mut members = Vec::new();
        for pending in self.entries.iter() {
            if pending.id == id {
                members.push(OcaMember::Order(pending.key));
            }
        }
        for pending in self.exits.iter() {
            if pending.id == id {
                members.push(OcaMember::Exit {
                    id: pending.id.clone(),
                    from_entry: pending.from_entry.clone(),
                    target_trade_key: pending.target_trade_key,
                });
            }
        }
        for pending in self.closes.iter() {
            if pending.public_id() == Some(id) {
                members.push(OcaMember::Order(pending.key));
            }
        }
        members
    }
}

impl Default for OrderBook {
    fn default() -> Self {
        Self::new()
    }
}
