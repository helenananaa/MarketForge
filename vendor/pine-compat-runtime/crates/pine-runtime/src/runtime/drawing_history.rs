use super::append_history::AppendHistory;
use super::id_store::IdStore;
use crate::{
    BoxOutput, BoxSnapshot, LabelOutput, LabelSnapshot, LineFillOutput, LineFillSnapshot,
    LineOutput, LineSnapshot, PineValue, PolylineOutput, PolylineSnapshot, TableOutput,
    TableSnapshot,
};

/// Retention can discard every display snapshot of a deleted object while
/// Pine variables still hold its handle. Identities are monotonic and never
/// reused within a checkpoint; the allocation frontier is its compact tombstone.
pub(crate) fn was_allocated(id: u32, next_id: u32) -> bool {
    id != 0 && id < next_id
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeDrawing<S> {
    pub(crate) id: u32,
    pub(crate) snapshots: AppendHistory<S>,
}

impl<S: Clone> RuntimeDrawing<S> {
    pub(crate) fn from_snapshot(id: u32, snapshot: S) -> Self {
        let mut snapshots = AppendHistory::default();
        snapshots.push(snapshot);
        Self { id, snapshots }
    }
}

/// Persistent drawing identities, ordered for public snapshots. Checkpoints
/// and streaming cursors share the root, including all retained history.
#[derive(Debug, Clone)]
pub(crate) struct DrawingStore<V>(IdStore<V>);

impl<V: Clone> Default for DrawingStore<V> {
    fn default() -> Self {
        Self(IdStore::new())
    }
}

pub(crate) trait DrawingIdentity {
    fn id(&self) -> u32;
}

impl<S> DrawingIdentity for RuntimeDrawing<S> {
    fn id(&self) -> u32 {
        self.id
    }
}

impl DrawingIdentity for RuntimeTable {
    fn id(&self) -> u32 {
        self.id
    }
}

impl<V: Clone + DrawingIdentity> DrawingStore<V> {
    pub(crate) fn push(&mut self, value: V) {
        self.0.insert(value.id(), value);
    }
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }
    pub(crate) fn capacity(&self) -> usize {
        self.0.capacity()
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &V> {
        self.0.values()
    }
    pub(crate) fn get(&self, id: u32) -> Option<&V> {
        self.0.get(&id)
    }
    pub(crate) fn get_mut(&mut self, id: u32) -> Option<&mut V> {
        self.0.get_mut(&id)
    }
    pub(crate) fn remove(&mut self, id: u32) {
        self.0.remove(id);
    }
    pub(crate) fn visit_differences(
        &self,
        previous: &Self,
        visit: impl FnMut(u32, Option<&V>, Option<&V>),
    ) -> usize {
        self.0.visit_differences(&previous.0, visit)
    }
}

pub(crate) fn drawing_by_id<S: Clone>(
    items: &DrawingStore<RuntimeDrawing<S>>,
    id: u32,
) -> Option<&RuntimeDrawing<S>> {
    items.get(id)
}

pub(crate) fn drawing_by_id_mut<S: Clone>(
    items: &mut DrawingStore<RuntimeDrawing<S>>,
    id: u32,
) -> Option<&mut RuntimeDrawing<S>> {
    items.get_mut(id)
}

pub(crate) type RuntimeLabel = RuntimeDrawing<LabelSnapshot>;
pub(crate) type RuntimeLine = RuntimeDrawing<LineSnapshot>;
pub(crate) type RuntimeLineFill = RuntimeDrawing<LineFillSnapshot>;
pub(crate) type RuntimePolyline = RuntimeDrawing<PolylineSnapshot>;
pub(crate) type RuntimeBox = RuntimeDrawing<BoxSnapshot>;

impl RuntimeLabel {
    pub(crate) fn snapshot(&self) -> LabelOutput {
        LabelOutput {
            id: self.id,
            snapshots: self.snapshots.to_vec(),
        }
    }
}

impl RuntimeLine {
    pub(crate) fn snapshot(&self) -> LineOutput {
        LineOutput {
            id: self.id,
            snapshots: self.snapshots.to_vec(),
        }
    }
}

impl RuntimeLineFill {
    pub(crate) fn snapshot(&self) -> LineFillOutput {
        LineFillOutput {
            id: self.id,
            snapshots: self.snapshots.to_vec(),
        }
    }
}

impl RuntimePolyline {
    pub(crate) fn snapshot(&self) -> PolylineOutput {
        PolylineOutput {
            id: self.id,
            snapshots: self.snapshots.to_vec(),
        }
    }
}

impl RuntimeBox {
    pub(crate) fn snapshot(&self) -> BoxOutput {
        BoxOutput {
            id: self.id,
            snapshots: self.snapshots.to_vec(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeTable {
    pub(crate) id: u32,
    pub(crate) position: PineValue,
    pub(crate) bg_color: PineValue,
    pub(crate) frame_color: PineValue,
    pub(crate) frame_width: PineValue,
    pub(crate) border_color: PineValue,
    pub(crate) border_width: PineValue,
    pub(crate) columns: i64,
    pub(crate) rows: i64,
    pub(crate) snapshots: AppendHistory<TableSnapshot>,
}

impl RuntimeTable {
    pub(crate) fn snapshot(&self) -> TableOutput {
        TableOutput {
            id: self.id,
            position: self.position.clone(),
            bg_color: self.bg_color.clone(),
            frame_color: self.frame_color.clone(),
            frame_width: self.frame_width.clone(),
            border_color: self.border_color.clone(),
            border_width: self.border_width.clone(),
            columns: self.columns,
            rows: self.rows,
            snapshots: self.snapshots.to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn retired_identity_frontier_does_not_accept_unallocated_handles() {
        assert!(!was_allocated(0, 20));
        assert!(was_allocated(1, 20));
        assert!(was_allocated(19, 20));
        assert!(!was_allocated(20, 20));
        assert!(!was_allocated(u32::MAX, 20));
        assert!(!was_allocated(1, 1));
    }

    #[test]
    fn drawing_store_checkpoints_clone_only_the_changed_object() {
        struct CountedDrawing {
            id: u32,
            clones: Arc<AtomicUsize>,
        }
        impl Clone for CountedDrawing {
            fn clone(&self) -> Self {
                self.clones.fetch_add(1, Ordering::Relaxed);
                Self {
                    id: self.id,
                    clones: self.clones.clone(),
                }
            }
        }
        impl DrawingIdentity for CountedDrawing {
            fn id(&self) -> u32 {
                self.id
            }
        }
        let clones = Arc::new(AtomicUsize::new(0));
        let mut store = DrawingStore::default();
        for id in 0..32_768 {
            store.push(CountedDrawing {
                id,
                clones: clones.clone(),
            });
        }
        let checkpoint = store.clone();
        let cursor = store.clone();
        assert_eq!(clones.load(Ordering::Relaxed), 0);
        store.get_mut(16_384).unwrap();
        assert_eq!(clones.load(Ordering::Relaxed), 1);
        let mut changed = Vec::new();
        assert!(store.visit_differences(&cursor, |id, _, _| changed.push(id)) <= 51);
        assert_eq!(changed, [16_384]);
        assert_eq!(checkpoint.len(), 32_768);
        store.remove(127);
        store.push(CountedDrawing {
            id: u32::MAX,
            clones,
        });
        assert_eq!(store.iter().last().unwrap().id, u32::MAX);
        assert_eq!(store.iter().nth(127).unwrap().id, 128);
    }

    #[test]
    fn drawing_checkpoint_append_copies_only_a_bounded_leaf() {
        #[derive(Debug)]
        struct Counted(Arc<AtomicUsize>);
        impl Clone for Counted {
            fn clone(&self) -> Self {
                self.0.fetch_add(1, Ordering::Relaxed);
                Self(self.0.clone())
            }
        }
        let count = Arc::new(AtomicUsize::new(0));
        let mut drawing = RuntimeDrawing::from_snapshot(1, Counted(count.clone()));
        for _ in 0..1_000 {
            drawing.snapshots.push(Counted(count.clone()));
        }
        let checkpoint = drawing.clone();
        count.store(0, Ordering::Relaxed);
        drawing.snapshots.push(Counted(count.clone()));
        assert!(count.load(Ordering::Relaxed) <= 128);
        assert_eq!(checkpoint.snapshots.len(), 1_001);
        assert_eq!(drawing.snapshots.len(), 1_002);
    }
}
