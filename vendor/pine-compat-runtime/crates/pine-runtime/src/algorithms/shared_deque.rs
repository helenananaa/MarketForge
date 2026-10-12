use std::{
    collections::VecDeque,
    ops::{Bound, Deref, DerefMut, Index, RangeBounds},
    sync::Arc,
};

const PAGE_SIZE: usize = 128;

#[derive(Debug)]
struct Page<T>(Vec<T>);

impl<T: Clone> Clone for Page<T> {
    fn clone(&self) -> Self {
        // Vec::clone uses capacity=len. An ensuing 127 -> 128 append would
        // double that to 254 and leave oversized pages throughout a sliding
        // queue. Allocate its fixed capacity once when making the page private.
        let mut values = Vec::with_capacity(PAGE_SIZE);
        values.extend(self.0.iter().cloned());
        Self(values)
    }
}

impl<T> Deref for Page<T> {
    type Target = Vec<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for Page<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Short queues keep their ordinary flat path. Larger checkpoints share the
/// complete interior pages while keeping the endpoints outside that directory.
/// Ordinary endpoint writes copy one bounded page; only page migrations copy
/// the directory, rather than copying its references for every checkpoint write.
#[derive(Debug, Clone)]
pub(crate) struct SharedDeque<T> {
    storage: Storage<T>,
}

#[derive(Debug, Clone)]
enum Storage<T> {
    Small(VecDeque<T>),
    Paged(PagedStorage<T>),
}

#[derive(Debug, Clone)]
struct PagedStorage<T> {
    front: Arc<Page<T>>,
    // A single page uses front for both ends, including any removed tail cells.
    back: Option<Arc<Page<T>>>,
    // This directory contains complete interior pages only. None avoids an
    // empty directory allocation for one or two pages and after shrinking.
    middle: Option<Arc<VecDeque<Arc<Page<T>>>>>,
    front_offset: usize,
    back_len: usize,
    len: usize,
}

impl<T> PagedStorage<T> {
    fn middle_len(&self) -> usize {
        self.middle.as_ref().map_or(0, |pages| pages.len())
    }

    fn page_count(&self) -> usize {
        1 + self.middle_len() + usize::from(self.back.is_some())
    }

    fn page(&self, index: usize) -> &Arc<Page<T>> {
        if index == 0 {
            &self.front
        } else if index <= self.middle_len() {
            &self.middle.as_ref().expect("interior page directory")[index - 1]
        } else {
            debug_assert_eq!(index, self.page_count() - 1);
            self.back.as_ref().expect("separate back page")
        }
    }

    // Only the endpoints may be partial. Every interior page has PAGE_SIZE
    // cells, so locating a value never walks the preceding pages.
    fn locate(&self, index: usize) -> (usize, usize) {
        let first = self.front.len() - self.front_offset;
        if index < first {
            (0, self.front_offset + index)
        } else {
            let rest = index - first;
            (1 + rest / PAGE_SIZE, rest % PAGE_SIZE)
        }
    }

    fn push_middle_front(&mut self, page: Arc<Page<T>>) {
        debug_assert_eq!(page.len(), PAGE_SIZE);
        let pages = self.middle.get_or_insert_with(|| Arc::new(VecDeque::new()));
        Arc::make_mut(pages).push_front(page);
    }

    fn push_middle_back(&mut self, page: Arc<Page<T>>) {
        debug_assert_eq!(page.len(), PAGE_SIZE);
        let pages = self.middle.get_or_insert_with(|| Arc::new(VecDeque::new()));
        Arc::make_mut(pages).push_back(page);
    }

    fn pop_middle_front(&mut self) -> Option<Arc<Page<T>>> {
        // Removing the sole interior page discards this branch's directory.
        // Copy its page handle directly instead of cloning a shared directory
        // that would immediately become empty and be dropped.
        if self.middle.as_ref()?.len() == 1 {
            return self.middle.take()?.front().cloned();
        }
        let pages = Arc::make_mut(self.middle.as_mut()?);
        let page = pages.pop_front();
        if pages.is_empty() {
            self.middle = None;
        } else {
            prune_directory(pages);
        }
        page
    }

    fn pop_middle_back(&mut self) -> Option<Arc<Page<T>>> {
        if self.middle.as_ref()?.len() == 1 {
            return self.middle.take()?.back().cloned();
        }
        let pages = Arc::make_mut(self.middle.as_mut()?);
        let page = pages.pop_back();
        if pages.is_empty() {
            self.middle = None;
        } else {
            prune_directory(pages);
        }
        page
    }
}

impl<T> Default for SharedDeque<T> {
    fn default() -> Self {
        Self {
            storage: Storage::Small(VecDeque::new()),
        }
    }
}

impl<T> SharedDeque<T> {
    pub(crate) fn len(&self) -> usize {
        match &self.storage {
            Storage::Small(values) => values.len(),
            Storage::Paged(pages) => pages.len,
        }
    }

    pub(crate) fn capacity(&self) -> usize {
        match &self.storage {
            Storage::Small(values) => values.capacity(),
            Storage::Paged(pages) => {
                // Every nonempty page reserves PAGE_SIZE cells, and endpoint
                // writes never grow its physical length beyond that reserve.
                // Count the buffers without visiting shared checkpoint pages.
                // ZST Vecs report usize::MAX; summing several would overflow.
                if std::mem::size_of::<T>() == 0 {
                    usize::MAX
                } else {
                    pages.page_count().saturating_mul(PAGE_SIZE)
                }
            }
        }
    }

    pub(crate) fn front(&self) -> Option<&T> {
        self.get(0)
    }

    pub(crate) fn back(&self) -> Option<&T> {
        self.len().checked_sub(1).and_then(|index| self.get(index))
    }

    pub(crate) fn get(&self, index: usize) -> Option<&T> {
        match &self.storage {
            Storage::Small(values) => values.get(index),
            Storage::Paged(pages) => {
                if index >= pages.len {
                    return None;
                }
                let (page, offset) = pages.locate(index);
                Some(&pages.page(page)[offset])
            }
        }
    }

    pub(crate) fn iter(&self) -> Iter<'_, T> {
        self.range(..)
    }

    pub(crate) fn range(&self, range: impl RangeBounds<usize>) -> Iter<'_, T> {
        match &self.storage {
            Storage::Small(values) => Iter::Small(values.range(range)),
            Storage::Paged(pages) => {
                let start = match range.start_bound() {
                    Bound::Included(&index) => index,
                    Bound::Excluded(&index) => index.checked_add(1).expect("range start overflow"),
                    Bound::Unbounded => 0,
                };
                let end = match range.end_bound() {
                    Bound::Included(&index) => index.checked_add(1).expect("range end overflow"),
                    Bound::Excluded(&index) => index,
                    Bound::Unbounded => pages.len,
                };
                assert!(
                    start <= end && end <= pages.len,
                    "deque range out of bounds"
                );
                let remaining = end - start;
                if remaining == 0 {
                    return Iter::Paged(PagedIter {
                        root: pages,
                        front_index: start,
                        back_index: end,
                        next_page: 0,
                        current: [].iter(),
                        reverse_page: 0,
                        reverse_current: None,
                        remaining: 0,
                    });
                }
                let (page, offset) = pages.locate(start);
                Iter::Paged(PagedIter {
                    root: pages,
                    front_index: start,
                    back_index: end,
                    next_page: page + 1,
                    current: pages.page(page)[offset..].iter(),
                    reverse_page: 0,
                    reverse_current: None,
                    remaining,
                })
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        match &mut self.storage {
            Storage::Small(values) => values.clear(),
            Storage::Paged(_) => *self = Self::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[cfg(test)]
    pub(crate) fn has_split_storage(&self) -> bool {
        match &self.storage {
            Storage::Small(values) => !values.as_slices().1.is_empty(),
            Storage::Paged(pages) => pages.page_count() > 1,
        }
    }

    #[cfg(test)]
    fn page_directory_capacity(&self) -> usize {
        match &self.storage {
            Storage::Small(_) => 0,
            Storage::Paged(pages) => pages.middle.as_ref().map_or(0, |pages| pages.capacity()),
        }
    }
}

fn prune_directory<T>(pages: &mut VecDeque<Arc<Page<T>>>) {
    // Repeated shrink/grow sessions must not retain a directory sized for the
    // session's historical maximum. Avoid reallocating at every page boundary.
    if pages.capacity() > 16 && pages.len() < pages.capacity() / 4 {
        pages.shrink_to(pages.len().saturating_mul(2).max(4));
    }
}

impl<T: Clone> SharedDeque<T> {
    fn promote(&mut self) {
        let Storage::Small(values) = &mut self.storage else {
            return;
        };
        let len = values.len();
        let mut values = std::mem::take(values).into_iter();
        let mut pages = VecDeque::with_capacity(len.div_ceil(PAGE_SIZE));
        while values.len() != 0 {
            let mut page = Vec::with_capacity(PAGE_SIZE);
            page.extend(values.by_ref().take(PAGE_SIZE));
            pages.push_back(Arc::new(Page(page)));
        }
        let back_len = pages.back().expect("nonempty promoted deque").len();
        let front = pages.pop_front().expect("nonempty promoted deque");
        let back = pages.pop_back();
        let middle = (!pages.is_empty()).then(|| Arc::new(pages));
        self.storage = Storage::Paged(PagedStorage {
            front,
            back,
            middle,
            front_offset: 0,
            back_len,
            len,
        });
    }

    pub(crate) fn push_back(&mut self, value: T) {
        if matches!(&self.storage, Storage::Small(values) if values.len() == PAGE_SIZE) {
            self.promote();
        }
        match &mut self.storage {
            Storage::Small(values) => values.push_back(value),
            Storage::Paged(pages) => {
                if pages.back_len == PAGE_SIZE {
                    let mut page = Vec::with_capacity(PAGE_SIZE);
                    page.push(value);
                    if let Some(back) = pages.back.take() {
                        pages.push_middle_back(back);
                    }
                    pages.back = Some(Arc::new(Page(page)));
                    pages.back_len = 1;
                } else {
                    let page = Arc::make_mut(pages.back.as_mut().unwrap_or(&mut pages.front));
                    if pages.back_len < page.len() {
                        page[pages.back_len] = value;
                    } else {
                        page.push(value);
                    }
                    pages.back_len += 1;
                }
                pages.len += 1;
            }
        }
    }

    pub(crate) fn push_front(&mut self, value: T) {
        if matches!(&self.storage, Storage::Small(values) if values.len() == PAGE_SIZE) {
            self.promote();
        }
        match &mut self.storage {
            Storage::Small(values) => values.push_front(value),
            Storage::Paged(pages) => {
                if pages.front_offset != 0 {
                    pages.front_offset -= 1;
                    Arc::make_mut(&mut pages.front)[pages.front_offset] = value;
                } else if pages.front.len() < PAGE_SIZE {
                    Arc::make_mut(&mut pages.front).insert(0, value);
                    if pages.back.is_none() {
                        pages.back_len += 1;
                    }
                } else {
                    let mut page = Vec::with_capacity(PAGE_SIZE);
                    page.push(value);
                    let previous = std::mem::replace(&mut pages.front, Arc::new(Page(page)));
                    if pages.back.is_none() {
                        // A single page can have a removed physical tail. It
                        // becomes the back endpoint with its logical back_len,
                        // rather than becoming a supposedly complete middle page.
                        pages.back = Some(previous);
                    } else {
                        pages.push_middle_front(previous);
                    }
                }
                pages.len += 1;
            }
        }
    }

    pub(crate) fn pop_front(&mut self) -> Option<T> {
        match &mut self.storage {
            Storage::Small(values) => values.pop_front(),
            Storage::Paged(pages) => {
                let value = pages.front[pages.front_offset].clone();
                pages.front_offset += 1;
                pages.len -= 1;
                if pages.len == 0 {
                    *self = Self::default();
                } else if pages.front_offset == pages.front.len() {
                    pages.front = pages
                        .pop_middle_front()
                        .unwrap_or_else(|| pages.back.take().expect("surviving back page"));
                    pages.front_offset = 0;
                }
                Some(value)
            }
        }
    }

    pub(crate) fn pop_back(&mut self) -> Option<T> {
        match &mut self.storage {
            Storage::Small(values) => values.pop_back(),
            Storage::Paged(pages) => {
                let value = pages.back.as_ref().unwrap_or(&pages.front)[pages.back_len - 1].clone();
                pages.back_len -= 1;
                pages.len -= 1;
                if pages.len == 0 {
                    *self = Self::default();
                } else if pages.back_len == 0 {
                    pages.back = pages.pop_middle_back();
                    pages.back_len = pages.back.as_ref().unwrap_or(&pages.front).len();
                }
                Some(value)
            }
        }
    }
}

impl SharedDeque<Option<f64>> {
    /// Restore an evicted sample at the front. A paged pop normally leaves the
    /// removed cell in place, so exact restoration only changes this queue's
    /// logical bounds; checkpoints can keep sharing both directory and page.
    /// Compare bits so signed zero and every NaN payload keep their identity.
    /// Other representations and changed cells use the ordinary write path.
    pub(crate) fn restore_front(&mut self, value: Option<f64>) {
        if let Storage::Paged(pages) = &mut self.storage
            && pages.front_offset != 0
            && pages.front[pages.front_offset - 1].map(f64::to_bits) == value.map(f64::to_bits)
        {
            pages.front_offset -= 1;
            pages.len += 1;
            return;
        }
        self.push_front(value);
    }
}

impl<T: Clone> From<VecDeque<T>> for SharedDeque<T> {
    fn from(values: VecDeque<T>) -> Self {
        let mut deque = Self {
            storage: Storage::Small(values),
        };
        if deque.len() > PAGE_SIZE {
            deque.promote();
        }
        deque
    }
}

impl<T: Clone> FromIterator<T> for SharedDeque<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut values = Self::default();
        for value in iter {
            values.push_back(value);
        }
        values
    }
}

impl<T: PartialEq> PartialEq for SharedDeque<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl<T: PartialEq> PartialEq<VecDeque<T>> for SharedDeque<T> {
    fn eq(&self, other: &VecDeque<T>) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl<T> Index<usize> for SharedDeque<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).expect("deque index out of bounds")
    }
}

impl<'a, T> IntoIterator for &'a SharedDeque<T> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub(crate) enum Iter<'a, T> {
    Small(std::collections::vec_deque::Iter<'a, T>),
    Paged(PagedIter<'a, T>),
}

pub(crate) struct PagedIter<'a, T> {
    root: &'a PagedStorage<T>,
    front_index: usize,
    back_index: usize,
    next_page: usize,
    current: std::slice::Iter<'a, T>,
    // Initialized only by reverse consumption. The logical remaining count
    // bounds both cursors even when their slices overlap or retain hidden cells.
    reverse_page: usize,
    reverse_current: Option<std::slice::Iter<'a, T>>,
    remaining: usize,
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Small(values) => values.next(),
            Self::Paged(iter) => {
                if iter.remaining == 0 {
                    return None;
                }
                loop {
                    if let Some(value) = iter.current.next() {
                        iter.remaining -= 1;
                        iter.front_index += 1;
                        return Some(value);
                    }
                    iter.current = iter.root.page(iter.next_page).iter();
                    iter.next_page += 1;
                }
            }
        }
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        match self {
            Self::Small(values) => values.nth(n),
            Self::Paged(iter) => {
                if n >= iter.remaining {
                    iter.front_index += iter.remaining;
                    iter.remaining = 0;
                    return None;
                }
                iter.front_index += n;
                iter.remaining -= n;
                let (page, offset) = iter.root.locate(iter.front_index);
                iter.current = iter.root.page(page)[offset..].iter();
                iter.next_page = page + 1;
                self.next()
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = match self {
            Self::Small(values) => values.len(),
            Self::Paged(iter) => iter.remaining,
        };
        (remaining, Some(remaining))
    }

    fn fold<B, F>(self, init: B, mut fold: F) -> B
    where
        F: FnMut(B, Self::Item) -> B,
    {
        match self {
            Self::Small(values) => values.fold(init, fold),
            Self::Paged(iter) => {
                // Carry the same accumulator across slices. Combining separate
                // per-page sums would change the floating-point addition order.
                let mut remaining = iter.remaining;
                let take = remaining.min(iter.current.len());
                let mut accumulator = iter.current.as_slice()[..take].iter().fold(init, &mut fold);
                remaining -= take;
                if remaining != 0
                    && let Some(pages) = &iter.root.middle
                {
                    let start = iter.next_page.saturating_sub(1).min(pages.len());
                    for page in pages.range(start..) {
                        if remaining == 0 {
                            break;
                        }
                        let take = remaining.min(page.len());
                        accumulator = page[..take].iter().fold(accumulator, &mut fold);
                        remaining -= take;
                    }
                }
                if remaining != 0 {
                    let page = iter.root.back.as_ref().expect("remaining back page");
                    let take = remaining.min(page.len());
                    accumulator = page[..take].iter().fold(accumulator, &mut fold);
                    remaining -= take;
                }
                debug_assert_eq!(remaining, 0);
                accumulator
            }
        }
    }
}

impl<T> DoubleEndedIterator for Iter<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::Small(values) => values.next_back(),
            Self::Paged(iter) => {
                if iter.remaining == 0 {
                    return None;
                }
                loop {
                    if let Some(current) = &mut iter.reverse_current {
                        if let Some(value) = current.next_back() {
                            iter.back_index -= 1;
                            iter.remaining -= 1;
                            return Some(value);
                        }
                        iter.reverse_page -= 1;
                        iter.reverse_current = Some(iter.root.page(iter.reverse_page).iter());
                    } else {
                        let (page, offset) = iter.root.locate(iter.back_index - 1);
                        iter.reverse_page = page;
                        iter.reverse_current = Some(iter.root.page(page)[..=offset].iter());
                    }
                }
            }
        }
    }

    fn nth_back(&mut self, n: usize) -> Option<Self::Item> {
        match self {
            Self::Small(values) => values.nth_back(n),
            Self::Paged(iter) => {
                if n >= iter.remaining {
                    iter.back_index -= iter.remaining;
                    iter.remaining = 0;
                    return None;
                }
                iter.back_index -= n;
                iter.remaining -= n;
                if n != 0 {
                    iter.reverse_current = None;
                }
                self.next_back()
            }
        }
    }
}

impl<T> ExactSizeIterator for Iter<'_, T> {}

#[cfg(test)]
#[path = "shared_deque_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "shared_deque_restore_tests.rs"]
mod restore_tests;

#[cfg(test)]
#[path = "shared_deque_endpoint_tests.rs"]
mod shared_deque_endpoint_tests;

#[cfg(test)]
#[path = "shared_deque_scan_tests.rs"]
mod shared_deque_scan_tests;

#[cfg(test)]
#[path = "shared_deque_reverse_tests.rs"]
mod shared_deque_reverse_tests;
