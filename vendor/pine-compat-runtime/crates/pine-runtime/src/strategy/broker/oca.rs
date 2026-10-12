use super::BrokerState;
use super::pending_exits::PendingExit;
use super::types::{OcaGroupKey, OcaMember, OcaType};

impl BrokerState {
    pub(crate) fn assign_pending_order_oca_named(
        &mut self,
        id: &str,
        name: String,
        oca_type: Option<&str>,
    ) {
        if name.is_empty() {
            return;
        }
        let oca_type = match oca_type {
            Some("strategy.oca.cancel") => OcaType::Cancel,
            Some("strategy.oca.reduce") => OcaType::Reduce,
            Some("strategy.oca.none") | None => OcaType::None,
            _ => return,
        };
        self.assign_pending_entry_oca(id, OcaGroupKey::new(name, oca_type));
    }

    #[allow(dead_code)]
    pub(super) fn assign_pending_entry_oca(&mut self, id: &str, group: OcaGroupKey) {
        let Some(pending) = self.order_book.entries().find_by_id(id) else {
            return;
        };
        let member = OcaMember::Order(pending.key);
        self.order_book.assign_oca(member, group);
    }

    #[allow(dead_code)]
    pub(super) fn pending_entry_oca(&self, id: &str) -> Option<&OcaGroupKey> {
        let pending = self.order_book.entries().find_by_id(id)?;
        self.order_book.oca_group(&OcaMember::Order(pending.key))
    }

    #[allow(dead_code)]
    pub(super) fn assign_pending_exit_oca(
        &mut self,
        id: &str,
        from_entry: &str,
        group: OcaGroupKey,
    ) {
        let Some(pending) = self.order_book.exits().find_by_identity(id, from_entry) else {
            return;
        };
        let member = OcaMember::Exit {
            id: pending.id.clone(),
            from_entry: pending.from_entry.clone(),
            target_trade_key: pending.target_trade_key,
        };
        self.order_book.assign_oca(member, group);
    }

    #[allow(dead_code)]
    pub(super) fn pending_exit_oca(&self, id: &str, from_entry: &str) -> Option<&OcaGroupKey> {
        let pending = self.order_book.exits().find_by_identity(id, from_entry)?;
        self.order_book.oca_group(&OcaMember::Exit {
            id: pending.id.clone(),
            from_entry: pending.from_entry.clone(),
            target_trade_key: pending.target_trade_key,
        })
    }

    pub(super) fn assign_placed_exit_oca(&mut self, id: &str, from_entry: &str) {
        let Some(name) = self.current_exit_oca_name().map(str::to_owned) else {
            return;
        };
        self.assign_pending_exit_oca(id, from_entry, OcaGroupKey::new(name, OcaType::Reduce));
    }

    pub(super) fn replace_all_exits_and_assign_oca(&mut self, pending_exits: Vec<PendingExit>) {
        let id = pending_exits.first().map(|pending| pending.id.clone());
        self.order_book.replace_all_exits(pending_exits);
        if let Some(id) = id {
            self.assign_placed_exits_oca(&id);
        }
    }

    fn assign_placed_exits_oca(&mut self, id: &str) {
        let Some(name) = self.current_exit_oca_name().map(str::to_owned) else {
            return;
        };
        let identities: Vec<(String, String, Option<u64>)> = self
            .order_book
            .exits()
            .iter()
            .filter(|pending| pending.id == id)
            .map(|pending| {
                (
                    pending.id.clone(),
                    pending.from_entry.clone(),
                    pending.target_trade_key,
                )
            })
            .collect();
        for (exit_id, from_entry, target_trade_key) in identities {
            self.order_book.assign_oca(
                OcaMember::Exit {
                    id: exit_id,
                    from_entry,
                    target_trade_key,
                },
                OcaGroupKey::new(name.clone(), OcaType::Reduce),
            );
        }
    }

    pub(super) fn placing_exit_oca_group(&self) -> Option<OcaGroupKey> {
        self.current_exit_oca_name()
            .map(|name| OcaGroupKey::new(name, OcaType::Reduce))
    }

    pub(super) fn reserved_quantity_excluding_oca_group(
        &self,
        entry_id: &str,
        released_identity: Option<(&str, &str)>,
        oca_group: Option<&OcaGroupKey>,
    ) -> f64 {
        self.order_book
            .exits()
            .iter()
            .filter(|pending_exit| pending_exit.from_entry == entry_id)
            .filter(|pending_exit| {
                released_identity.is_none_or(|(id, from_entry)| {
                    pending_exit.id != id || pending_exit.from_entry != from_entry
                })
            })
            .filter(|pending_exit| {
                let member = OcaMember::Exit {
                    id: pending_exit.id.clone(),
                    from_entry: pending_exit.from_entry.clone(),
                    target_trade_key: pending_exit.target_trade_key,
                };
                match (oca_group, self.order_book.oca_group(&member)) {
                    (Some(placing), Some(existing)) => placing != existing,
                    _ => true,
                }
            })
            .map(|pending_exit| pending_exit.reserved_quantity)
            .filter(|reserved_quantity| reserved_quantity.is_finite() && *reserved_quantity > 0.0)
            .sum()
    }

    pub(super) fn has_same_exit_oca_group_peer(
        &self,
        released_identity: Option<(&str, &str)>,
        oca_group: &OcaGroupKey,
    ) -> bool {
        self.order_book.exits().iter().any(|pending_exit| {
            if released_identity
                .is_some_and(|(id, from)| pending_exit.id == id && pending_exit.from_entry == from)
            {
                return false;
            }
            self.order_book.oca_group(&OcaMember::Exit {
                id: pending_exit.id.clone(),
                from_entry: pending_exit.from_entry.clone(),
                target_trade_key: pending_exit.target_trade_key,
            }) == Some(oca_group)
        })
    }
}
