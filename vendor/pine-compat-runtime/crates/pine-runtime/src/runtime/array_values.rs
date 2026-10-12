//! Array payloads keep small arrays flat and share pages in large checkpoints.
//! A sparse write copies one page and a bounded directory path.
use std::{
    ops::{Index, IndexMut},
    sync::Arc,
};

const PAGE_SIZE: usize = 128;

#[path = "array_values/page_directory.rs"]
mod page_directory;
use page_directory::PageDirectory;

#[derive(Clone, Debug)]
pub(crate) struct ArrayValues<T = crate::PineValue> {
    storage: Storage<T>,
}

#[derive(Clone, Debug)]
enum Storage<T> {
    Small(Vec<T>),
    Paged { pages: PageDirectory<T>, len: usize },
}

impl<T> From<Vec<T>> for ArrayValues<T> {
    fn from(values: Vec<T>) -> Self {
        let len = values.len();
        let storage = if len <= PAGE_SIZE {
            Storage::Small(values)
        } else {
            let mut values = values.into_iter();
            let mut pages = Vec::with_capacity(len.div_ceil(PAGE_SIZE));
            while values.len() != 0 {
                pages.push(Arc::new(values.by_ref().take(PAGE_SIZE).collect()));
            }
            Storage::Paged {
                pages: PageDirectory::from_pages(pages),
                len,
            }
        };
        Self { storage }
    }
}

impl<T> ArrayValues<T> {
    pub(crate) fn len(&self) -> usize {
        match &self.storage {
            Storage::Small(values) => values.len(),
            Storage::Paged { len, .. } => *len,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn capacity(&self) -> usize {
        match &self.storage {
            Storage::Small(values) => values.capacity(),
            Storage::Paged { pages, .. } => pages.iter().map(|page| page.capacity()).sum(),
        }
    }

    pub(crate) fn get(&self, index: usize) -> Option<&T> {
        match &self.storage {
            Storage::Small(values) => values.get(index),
            Storage::Paged { pages, len } => {
                (index < *len).then(|| &pages.get(index / PAGE_SIZE)[index % PAGE_SIZE])
            }
        }
    }

    pub(crate) fn view(&self, start: usize, len: usize) -> ArrayView<'_, T> {
        assert!(start <= self.len() && len <= self.len() - start);
        ArrayView {
            values: self,
            start,
            len,
        }
    }

    pub(crate) fn iter(&self) -> ArrayIter<'_, T> {
        self.view(0, self.len()).iter()
    }

    /// Borrow the logical payload as contiguous slices, visiting pages once.
    pub(crate) fn slices(&self) -> impl Iterator<Item = &[T]> {
        match &self.storage {
            Storage::Small(values) => ArraySlices::Small(Some(values.as_slice())),
            Storage::Paged { pages, len } => ArraySlices::Paged {
                pages: pages.iter(),
                remaining: *len,
            },
        }
    }

    /// Cells copied by cloning this payload; paged clones share their directory.
    pub(crate) fn clone_allocation_values(&self) -> &[T] {
        match &self.storage {
            Storage::Small(values) => values,
            Storage::Paged { .. } => &[],
        }
    }

    /// An append only copies an existing page when that partial tail is shared.
    pub(crate) fn append_allocation_values(&self, cloned_self: bool) -> &[T] {
        match &self.storage {
            Storage::Small(values) if cloned_self => values,
            Storage::Paged { len, .. } if !len.is_multiple_of(PAGE_SIZE) => {
                self.write_allocation_values(*len - 1, cloned_self)
            }
            _ => &[],
        }
    }

    /// Cells copied by mutating every page; uniquely owned pages are skipped.
    pub(crate) fn all_write_allocation_values(
        &self,
        cloned_self: bool,
    ) -> impl Iterator<Item = &T> {
        let small = match &self.storage {
            Storage::Small(values) if cloned_self => Some(values.as_slice()),
            _ => None,
        };
        let pages = match &self.storage {
            Storage::Paged { pages, .. } => Some(pages),
            _ => None,
        };
        small.into_iter().flatten().chain(
            pages
                .into_iter()
                .flat_map(move |pages| pages.copied_values(cloned_self)),
        )
    }

    /// Values deep-cloned by a sparse write after an optional store-entry
    /// clone. Large payloads expose only the affected bounded page.
    pub(crate) fn write_allocation_values(&self, index: usize, cloned_self: bool) -> &[T] {
        match &self.storage {
            Storage::Small(values) if cloned_self => values,
            Storage::Paged { pages, len }
                if index < *len || (index == *len && !len.is_multiple_of(PAGE_SIZE)) =>
            {
                pages
                    .write_page(index / PAGE_SIZE, cloned_self)
                    .unwrap_or(&[])
            }
            _ => &[],
        }
    }
}

impl<T: Clone> ArrayValues<T> {
    pub(crate) fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        match &mut self.storage {
            Storage::Small(values) => values.get_mut(index),
            Storage::Paged { pages, len } => {
                if index >= *len {
                    return None;
                }
                Arc::make_mut(pages.get_mut(index / PAGE_SIZE)).get_mut(index % PAGE_SIZE)
            }
        }
    }

    pub(crate) fn to_vec(&self) -> Vec<T> {
        let mut values = Vec::with_capacity(self.len());
        values.extend(self.slices().flatten().cloned());
        values
    }

    pub(crate) fn fill(&mut self, value: T) {
        match &mut self.storage {
            Storage::Small(values) => values.fill(value),
            Storage::Paged { pages, .. } => {
                pages.for_each_mut(|page| {
                    if let Some(values) = Arc::get_mut(page) {
                        values.fill(value.clone());
                    } else {
                        // A full overwrite does not need to clone old cells.
                        *page = Arc::new(vec![value.clone(); page.len()]);
                    }
                });
            }
        }
    }

    pub(crate) fn swap(&mut self, left: usize, right: usize) {
        assert!(left < self.len() && right < self.len());
        if left == right {
            return;
        }
        match &mut self.storage {
            Storage::Small(values) => values.swap(left, right),
            Storage::Paged { pages, .. } => {
                let (left_page, right_page) = (left / PAGE_SIZE, right / PAGE_SIZE);
                if left_page == right_page {
                    Arc::make_mut(pages.get_mut(left_page))
                        .swap(left % PAGE_SIZE, right % PAGE_SIZE);
                } else {
                    let (low, high, low_offset, high_offset) = if left_page < right_page {
                        (left_page, right_page, left % PAGE_SIZE, right % PAGE_SIZE)
                    } else {
                        (right_page, left_page, right % PAGE_SIZE, left % PAGE_SIZE)
                    };
                    let (low_page, high_page) = pages.two_mut(low, high);
                    std::mem::swap(
                        &mut Arc::make_mut(low_page)[low_offset],
                        &mut Arc::make_mut(high_page)[high_offset],
                    );
                }
            }
        }
    }

    pub(crate) fn reverse(&mut self) {
        match &mut self.storage {
            Storage::Small(values) => values.reverse(),
            Storage::Paged { pages, len } => {
                if *len < 2 {
                    return;
                }
                // Resolve the directory once. Each pair of contiguous chunks
                // then swaps cells through borrowed slices, including a partial
                // final page, instead of walking the tree for every cell pair.
                let mut pages = pages.mutable_pages();
                let (mut left, mut right) = (0, *len);
                while right - left > 1 {
                    let left_page = left / PAGE_SIZE;
                    let right_page = (right - 1) / PAGE_SIZE;
                    let left_offset = left % PAGE_SIZE;
                    let right_offset = (right - 1) % PAGE_SIZE + 1;
                    if left_page == right_page {
                        Arc::make_mut(&mut *pages[left_page])[left_offset..right_offset].reverse();
                        break;
                    }
                    let count = (PAGE_SIZE - left_offset)
                        .min(right_offset)
                        .min((right - left) / 2);
                    let (prefix, suffix) = pages.split_at_mut(right_page);
                    let low = Arc::make_mut(&mut *prefix[left_page]);
                    let high = Arc::make_mut(&mut *suffix[0]);
                    for (low, high) in low[left_offset..left_offset + count]
                        .iter_mut()
                        .zip(high[right_offset - count..right_offset].iter_mut().rev())
                    {
                        std::mem::swap(low, high);
                    }
                    left += count;
                    right -= count;
                }
            }
        }
    }

    pub(crate) fn extend(&mut self, values: impl IntoIterator<Item = T>) {
        for value in values {
            self.insert(self.len(), value);
        }
    }

    #[cfg(test)]
    pub(crate) fn replace_range(
        &mut self,
        start: usize,
        end: usize,
        values: impl IntoIterator<Item = T>,
    ) {
        assert!(start <= end && end <= self.len());
        if start == self.len() {
            self.extend(values);
            return;
        }
        let values = values.into_iter();
        let mut next = Vec::with_capacity(self.len() - (end - start) + values.size_hint().0);
        next.extend(self.view(0, start).iter().cloned());
        next.extend(values);
        next.extend(self.view(end, self.len() - end).iter().cloned());
        *self = next.into();
    }

    /// Builds one replacement payload for a middle insertion. The caller can
    /// replace a shared store entry directly instead of cloning it before
    /// discarding its old contents.
    pub(crate) fn with_inserted(&self, index: usize, values: Vec<T>) -> Self {
        assert!(index <= self.len());
        let mut next = Vec::with_capacity(self.len() + values.len());
        next.extend(self.view(0, index).iter().cloned());
        next.extend(values);
        next.extend(self.view(index, self.len() - index).iter().cloned());
        next.into()
    }

    pub(crate) fn insert(&mut self, index: usize, value: T) {
        let len = self.len();
        assert!(index <= len);
        // Middle insertions need to shift the suffix. Keep their owned, flat
        // operation separate from the common checkpoint + tail append path.
        if index != len {
            if let Storage::Small(values) = &mut self.storage {
                values.insert(index, value);
                if values.len() > PAGE_SIZE {
                    *self = std::mem::take(values).into();
                }
                return;
            }
            let mut values = self.to_vec();
            values.insert(index, value);
            *self = values.into();
            return;
        }
        match &mut self.storage {
            Storage::Small(values) if values.len() < PAGE_SIZE => values.push(value),
            Storage::Small(values) => {
                let first = Arc::new(std::mem::take(values));
                self.storage = Storage::Paged {
                    pages: PageDirectory::from_pages(vec![first, Arc::new(vec![value])]),
                    len: len + 1,
                };
            }
            Storage::Paged { pages, len } => {
                if *len % PAGE_SIZE == 0 {
                    pages.push(Arc::new(vec![value]));
                } else {
                    Arc::make_mut(pages.get_mut((*len - 1) / PAGE_SIZE)).push(value);
                }
                *len += 1;
            }
        }
    }

    pub(crate) fn remove(&mut self, index: usize) -> T {
        let len = self.len();
        assert!(index < len);
        if index + 1 != len {
            if let Storage::Small(values) = &mut self.storage {
                return values.remove(index);
            }
            let mut values = self.to_vec();
            let removed = values.remove(index);
            *self = values.into();
            return removed;
        }
        match &mut self.storage {
            Storage::Small(values) => values.pop().expect("nonempty array"),
            Storage::Paged { pages, len } => {
                let last = Arc::make_mut(pages.get_mut((*len - 1) / PAGE_SIZE));
                let removed = last.pop().expect("nonempty page");
                if last.is_empty() {
                    pages.pop();
                }
                *len -= 1;
                if *len == 0 {
                    self.storage = Storage::Small(Vec::new());
                }
                removed
            }
        }
    }
}

impl<T: PartialEq> PartialEq for ArrayValues<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl<T: Eq> Eq for ArrayValues<T> {}

impl<T> Index<usize> for ArrayValues<T> {
    type Output = T;
    fn index(&self, index: usize) -> &T {
        self.get(index).expect("array values index")
    }
}

impl<T: Clone> IndexMut<usize> for ArrayValues<T> {
    fn index_mut(&mut self, index: usize) -> &mut T {
        self.get_mut(index).expect("array values index")
    }
}

pub(crate) struct ArrayView<'a, T = crate::PineValue> {
    values: &'a ArrayValues<T>,
    start: usize,
    len: usize,
}

impl<'a, T> ArrayView<'a, T> {
    pub(crate) fn len(&self) -> usize {
        self.len
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub(crate) fn get(&self, index: usize) -> Option<&'a T> {
        (index < self.len).then(|| self.values.get(self.start + index).expect("valid view"))
    }
    pub(crate) fn iter(&self) -> ArrayIter<'a, T> {
        ArrayIter {
            values: self.values,
            front: self.start,
            back: self.start + self.len,
            current: [].iter(),
            reverse_current: [].iter(),
        }
    }
}

impl<T> Index<usize> for ArrayView<'_, T> {
    type Output = T;
    fn index(&self, index: usize) -> &T {
        self.get(index).expect("array view index")
    }
}

impl<'a, T> IntoIterator for ArrayView<'a, T> {
    type Item = &'a T;
    type IntoIter = ArrayIter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T> IntoIterator for &'a ArrayValues<T> {
    type Item = &'a T;
    type IntoIter = ArrayIter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

enum ArraySlices<'a, T> {
    Small(Option<&'a [T]>),
    Paged {
        pages: page_directory::Pages<'a, T>,
        remaining: usize,
    },
}

impl<'a, T> Iterator for ArraySlices<'a, T> {
    type Item = &'a [T];

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Small(values) => values.take(),
            Self::Paged { pages, remaining } => {
                if *remaining == 0 {
                    return None;
                }
                let page = pages.next().expect("valid paged array length");
                let visible = page.len().min(*remaining);
                *remaining -= visible;
                Some(&page[..visible])
            }
        }
    }
}

pub(crate) struct ArrayIter<'a, T> {
    values: &'a ArrayValues<T>,
    front: usize,
    back: usize,
    current: std::slice::Iter<'a, T>,
    reverse_current: std::slice::Iter<'a, T>,
}

impl<'a, T> Iterator for ArrayIter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }
        if self.current.as_slice().is_empty() {
            self.current = match &self.values.storage {
                Storage::Small(values) => values[self.front..].iter(),
                Storage::Paged { pages, .. } => {
                    pages.get(self.front / PAGE_SIZE)[self.front % PAGE_SIZE..].iter()
                }
            };
        }
        self.front += 1;
        self.current.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.back - self.front;
        (len, Some(len))
    }
}

impl<T> DoubleEndedIterator for ArrayIter<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }
        if self.reverse_current.as_slice().is_empty() {
            let index = self.back - 1;
            self.reverse_current = match &self.values.storage {
                Storage::Small(values) => values[..=index].iter(),
                Storage::Paged { pages, .. } => {
                    pages.get(index / PAGE_SIZE)[..=index % PAGE_SIZE].iter()
                }
            };
        }
        self.back -= 1;
        self.reverse_current.next_back()
    }
}
impl<T> ExactSizeIterator for ArrayIter<'_, T> {}

#[cfg(test)]
mod tests;
