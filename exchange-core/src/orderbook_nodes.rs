//! Order IDs index small slot handles. Large order nodes live in contiguous
//! persistent chunks, so a FIFO sweep copies touched chunks rather than large
//! values scattered throughout an ID tree. Released slots are reused locally;
//! cloned books retain their own chunk and free-list versions.
use super::{OrderId, OrderNode};
use im::{OrdMap, Vector};
use std::{num::NonZeroUsize, ops::Index};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NodeSlot(NonZeroUsize);

impl NodeSlot {
    fn index(self) -> usize {
        self.0.get() - 1
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct OrderNodes {
    by_id: OrdMap<OrderId, NodeSlot>,
    slots: Vector<Option<OrderNode>>,
    free: Vector<NodeSlot>,
}

impl OrderNodes {
    pub(super) fn len(&self) -> usize {
        self.by_id.len()
    }

    pub(super) fn contains_key(&self, id: &OrderId) -> bool {
        self.by_id.contains_key(id)
    }

    pub(super) fn slot(&self, id: &OrderId) -> Option<NodeSlot> {
        self.by_id.get(id).copied()
    }

    pub(super) fn get(&self, id: &OrderId) -> Option<&OrderNode> {
        self.at(self.slot(id)?)
    }

    pub(super) fn at(&self, slot: NodeSlot) -> Option<&OrderNode> {
        self.slots.get(slot.index())?.as_ref()
    }

    pub(super) fn at_mut(&mut self, slot: NodeSlot) -> Option<&mut OrderNode> {
        self.slots.get_mut(slot.index())?.as_mut()
    }

    pub(super) fn insert(&mut self, id: OrderId, node: OrderNode) -> NodeSlot {
        debug_assert!(!self.contains_key(&id));
        let slot = if let Some(slot) = self.free.pop_back() {
            debug_assert!(self.slots[slot.index()].is_none());
            self.slots.set(slot.index(), Some(node));
            slot
        } else {
            let slot = NodeSlot(NonZeroUsize::new(self.slots.len() + 1).expect("node capacity"));
            self.slots.push_back(Some(node));
            slot
        };
        self.by_id.insert(id, slot);
        slot
    }

    pub(super) fn remove(&mut self, slot: NodeSlot) -> Option<OrderNode> {
        let node = self.slots.get_mut(slot.index())?.take()?;
        self.by_id.remove(&node.order.order_id);
        self.free.push_back(slot);
        Some(node)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&OrderId, &OrderNode)> {
        self.by_id
            .iter()
            .map(|(id, slot)| (id, self.at(*slot).expect("live ID slot")))
    }

    #[cfg(test)]
    pub(super) fn ptr_eq(&self, other: &Self) -> bool {
        self.by_id.ptr_eq(&other.by_id) && self.slots.ptr_eq(&other.slots)
    }

    #[cfg(test)]
    pub(super) fn allocated_slots(&self) -> usize {
        self.slots.len()
    }
}

impl Index<&OrderId> for OrderNodes {
    type Output = OrderNode;

    fn index(&self, id: &OrderId) -> &Self::Output {
        self.get(id).expect("live order ID")
    }
}
