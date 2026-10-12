#![allow(dead_code)]

use super::types::{InternalOrderKey, StrategyCommandOrigin, StrategyOrderMetadata};

#[derive(Debug, Clone, PartialEq)]
pub(super) enum PendingCloseKind {
    Close { id: String },
    CloseAll,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PendingCloseQuantity {
    Full,
    Qty(f64),
    QtyPercent(f64),
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PendingClose {
    pub(super) key: InternalOrderKey,
    pub(super) origin: StrategyCommandOrigin,
    pub(super) kind: PendingCloseKind,
    pub(super) quantity: PendingCloseQuantity,
    pub(super) created_bar_index: usize,
    pub(super) immediately: bool,
    pub(super) metadata: StrategyOrderMetadata,
}

impl PendingClose {
    pub(super) fn public_id(&self) -> Option<&str> {
        match &self.kind {
            PendingCloseKind::Close { id } => Some(id.as_str()),
            PendingCloseKind::CloseAll => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct PendingCloseBook {
    closes: Vec<PendingClose>,
}

impl PendingCloseBook {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &PendingClose> {
        self.closes.iter()
    }

    pub(super) fn count(&self) -> usize {
        self.closes.len()
    }

    pub(super) fn find_close_by_id(&self, id: &str) -> Option<&PendingClose> {
        self.closes.iter().find(|pending| match &pending.kind {
            PendingCloseKind::Close { id: pending_id } => pending_id == id,
            PendingCloseKind::CloseAll => false,
        })
    }

    pub(super) fn place(
        &mut self,
        mut pending: PendingClose,
        allocate: &mut dyn FnMut() -> InternalOrderKey,
    ) {
        let existing_key = match &pending.kind {
            PendingCloseKind::Close { id } => self
                .closes
                .iter()
                .find(|existing| existing.public_id() == Some(id.as_str()))
                .map(|existing| existing.key),
            PendingCloseKind::CloseAll => self
                .closes
                .iter()
                .find(|existing| matches!(existing.kind, PendingCloseKind::CloseAll))
                .map(|existing| existing.key),
        };
        let key = existing_key.unwrap_or_else(allocate);
        pending.key = key;
        match &pending.kind {
            PendingCloseKind::Close { id } => {
                if let Some(existing) = self
                    .closes
                    .iter_mut()
                    .find(|existing| existing.public_id() == Some(id.as_str()))
                {
                    *existing = pending;
                    return;
                }
            }
            PendingCloseKind::CloseAll => {
                if let Some(existing) = self
                    .closes
                    .iter_mut()
                    .find(|existing| matches!(existing.kind, PendingCloseKind::CloseAll))
                {
                    *existing = pending;
                    return;
                }
            }
        }
        self.closes.push(pending);
    }

    pub(super) fn cancel_id(&mut self, id: &str) {
        self.closes
            .retain(|pending| pending.public_id() != Some(id));
    }

    pub(super) fn clear_all(&mut self) {
        self.closes.clear();
    }

    pub(super) fn take_eligible(&mut self, bar_index: usize) -> Vec<PendingClose> {
        let mut eligible = Vec::new();
        let mut index = 0;
        while index < self.closes.len() {
            if self.closes[index].created_bar_index < bar_index {
                eligible.push(self.closes.remove(index));
            } else {
                index += 1;
            }
        }
        eligible
    }

    pub(super) fn take_same_bar(&mut self, bar_index: usize) -> Vec<PendingClose> {
        let mut same_bar = Vec::new();
        let mut index = 0;
        while index < self.closes.len() {
            if self.closes[index].created_bar_index == bar_index && !self.closes[index].immediately
            {
                same_bar.push(self.closes.remove(index));
            } else {
                index += 1;
            }
        }
        same_bar
    }

    pub(super) fn take_immediate(&mut self) -> Vec<PendingClose> {
        let mut immediate = Vec::new();
        let mut index = 0;
        while index < self.closes.len() {
            if self.closes[index].immediately {
                immediate.push(self.closes.remove(index));
            } else {
                index += 1;
            }
        }
        immediate
    }
}
