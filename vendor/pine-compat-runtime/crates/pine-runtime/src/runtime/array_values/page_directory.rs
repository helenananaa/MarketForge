//! A persistent radix directory for array payload pages.
//!
//! Sparse writes copy bounded directory nodes rather than every page handle.
//! Payload pages keep their existing ownership and capacity contracts.
use std::{ops::Range, sync::Arc};

const FANOUT_SHIFT: u32 = 5;
const FANOUT: usize = 1 << FANOUT_SHIFT;

#[derive(Debug)]
pub(super) struct PageDirectory<T> {
    root: Arc<Node<T>>,
    len: usize,
}

#[derive(Debug)]
enum Node<T> {
    Leaf(Vec<Arc<Vec<T>>>),
    Branch {
        child_shift: u32,
        children: Vec<Arc<Node<T>>>,
    },
}

impl<T> Clone for Node<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Leaf(pages) => Self::Leaf(pages.clone()),
            Self::Branch {
                child_shift,
                children,
            } => Self::Branch {
                child_shift: *child_shift,
                children: children.clone(),
            },
        }
    }
}

impl<T> Clone for PageDirectory<T> {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            len: self.len,
        }
    }
}

impl<T> PageDirectory<T> {
    pub(super) fn from_pages(pages: Vec<Arc<Vec<T>>>) -> Self {
        let len = pages.len();
        let mut pages = pages.into_iter();
        let mut nodes = Vec::new();
        while pages.len() != 0 {
            nodes.push(Arc::new(Node::Leaf(pages.by_ref().take(FANOUT).collect())));
        }
        if nodes.is_empty() {
            nodes.push(Arc::new(Node::Leaf(Vec::new())));
        }
        let mut shift = FANOUT_SHIFT;
        while nodes.len() > 1 {
            let mut previous = nodes.into_iter();
            nodes = Vec::new();
            while previous.len() != 0 {
                nodes.push(Arc::new(Node::Branch {
                    child_shift: shift,
                    children: previous.by_ref().take(FANOUT).collect(),
                }));
            }
            shift += FANOUT_SHIFT;
        }
        Self {
            root: nodes.pop().expect("directory root"),
            len,
        }
    }

    pub(super) fn get(&self, index: usize) -> &Arc<Vec<T>> {
        assert!(index < self.len);
        let mut node = self.root.as_ref();
        let mut index = index;
        loop {
            match node {
                Node::Leaf(pages) => return &pages[index],
                Node::Branch {
                    child_shift,
                    children,
                } => {
                    node = &children[index >> child_shift];
                    index &= child_mask(*child_shift);
                }
            }
        }
    }

    /// Sharing at any ancestor forces the page handle to be copied on a write,
    /// even if the leaf and payload currently have only one direct Arc owner.
    pub(super) fn write_page(&self, index: usize, cloned_self: bool) -> Option<&[T]> {
        assert!(index < self.len);
        let mut node = &self.root;
        let mut index = index;
        let mut shared = cloned_self;
        loop {
            shared |= Arc::strong_count(node) > 1;
            match node.as_ref() {
                Node::Leaf(pages) => {
                    let page = &pages[index];
                    return (shared || Arc::strong_count(page) > 1).then_some(page.as_slice());
                }
                Node::Branch {
                    child_shift,
                    children,
                } => {
                    node = &children[index >> child_shift];
                    index &= child_mask(*child_shift);
                }
            }
        }
    }

    pub(super) fn iter(&self) -> Pages<'_, T> {
        Pages {
            directory: self,
            indices: 0..self.len,
            current: [].iter(),
        }
    }

    fn leaf_suffix(&self, mut index: usize) -> &[Arc<Vec<T>>] {
        let mut node = self.root.as_ref();
        loop {
            match node {
                Node::Leaf(pages) => return &pages[index..],
                Node::Branch {
                    child_shift,
                    children,
                } => {
                    node = &children[index >> child_shift];
                    index &= child_mask(*child_shift);
                }
            }
        }
    }

    pub(super) fn copied_values(&self, cloned_self: bool) -> impl Iterator<Item = &T> {
        (0..self.len)
            .filter_map(move |index| self.write_page(index, cloned_self))
            .flatten()
    }

    pub(super) fn get_mut(&mut self, index: usize) -> &mut Arc<Vec<T>> {
        assert!(index < self.len);
        page_mut(&mut self.root, index)
    }

    pub(super) fn two_mut(
        &mut self,
        left: usize,
        right: usize,
    ) -> (&mut Arc<Vec<T>>, &mut Arc<Vec<T>>) {
        assert!(left < right && right < self.len);
        two_pages_mut(&mut self.root, left, right)
    }

    pub(super) fn for_each_mut(&mut self, mut visit: impl FnMut(&mut Arc<Vec<T>>)) {
        visit_pages_mut(&mut self.root, &mut visit);
    }

    /// Borrow all pages after copying shared directory nodes once. The scratch
    /// list contains references, not cloned page handles or payload values.
    pub(super) fn mutable_pages(&mut self) -> Vec<&mut Arc<Vec<T>>> {
        let mut pages = Vec::with_capacity(self.len);
        collect_pages_mut(&mut self.root, &mut pages);
        pages
    }

    pub(super) fn push(&mut self, page: Arc<Vec<T>>) {
        let span = match self.root.as_ref() {
            Node::Leaf(_) => FANOUT,
            Node::Branch { child_shift, .. } => 1_usize
                .checked_shl(child_shift + FANOUT_SHIFT)
                .expect("array directory span"),
        };
        if self.len == span {
            self.root = Arc::new(Node::Branch {
                child_shift: span.trailing_zeros(),
                children: vec![self.root.clone()],
            });
        }
        push_page(&mut self.root, self.len, page);
        self.len += 1;
    }

    pub(super) fn pop(&mut self) {
        assert!(self.len != 0);
        pop_page(&mut self.root);
        self.len -= 1;
        while let Node::Branch { children, .. } = self.root.as_ref() {
            if children.len() != 1 {
                break;
            }
            self.root = children[0].clone();
        }
    }
}

fn child_mask(shift: u32) -> usize {
    (1_usize << shift) - 1
}

fn page_mut<T>(node: &mut Arc<Node<T>>, index: usize) -> &mut Arc<Vec<T>> {
    match Arc::make_mut(node) {
        Node::Leaf(pages) => &mut pages[index],
        Node::Branch {
            child_shift,
            children,
        } => page_mut(
            &mut children[index >> *child_shift],
            index & child_mask(*child_shift),
        ),
    }
}

fn two_pages_mut<T>(
    node: &mut Arc<Node<T>>,
    left: usize,
    right: usize,
) -> (&mut Arc<Vec<T>>, &mut Arc<Vec<T>>) {
    match Arc::make_mut(node) {
        Node::Leaf(pages) => {
            let (prefix, suffix) = pages.split_at_mut(right);
            (&mut prefix[left], &mut suffix[0])
        }
        Node::Branch {
            child_shift,
            children,
        } => {
            let (low, high) = (left >> *child_shift, right >> *child_shift);
            if low == high {
                two_pages_mut(
                    &mut children[low],
                    left & child_mask(*child_shift),
                    right & child_mask(*child_shift),
                )
            } else {
                let (prefix, suffix) = children.split_at_mut(high);
                (
                    page_mut(&mut prefix[low], left & child_mask(*child_shift)),
                    page_mut(&mut suffix[0], right & child_mask(*child_shift)),
                )
            }
        }
    }
}

fn visit_pages_mut<T>(node: &mut Arc<Node<T>>, visit: &mut impl FnMut(&mut Arc<Vec<T>>)) {
    match Arc::make_mut(node) {
        Node::Leaf(pages) => pages.iter_mut().for_each(visit),
        Node::Branch { children, .. } => children
            .iter_mut()
            .for_each(|child| visit_pages_mut(child, visit)),
    }
}

fn collect_pages_mut<'a, T>(node: &'a mut Arc<Node<T>>, pages: &mut Vec<&'a mut Arc<Vec<T>>>) {
    match Arc::make_mut(node) {
        Node::Leaf(values) => pages.extend(values.iter_mut()),
        Node::Branch { children, .. } => {
            for child in children {
                collect_pages_mut(child, pages);
            }
        }
    }
}

fn single_path<T>(shift: u32, page: Arc<Vec<T>>) -> Arc<Node<T>> {
    Arc::new(if shift == FANOUT_SHIFT {
        Node::Leaf(vec![page])
    } else {
        Node::Branch {
            child_shift: shift - FANOUT_SHIFT,
            children: vec![single_path(shift - FANOUT_SHIFT, page)],
        }
    })
}

fn push_page<T>(node: &mut Arc<Node<T>>, index: usize, page: Arc<Vec<T>>) {
    match Arc::make_mut(node) {
        Node::Leaf(pages) => {
            debug_assert_eq!(index, pages.len());
            pages.push(page);
        }
        Node::Branch {
            child_shift,
            children,
        } => {
            let child = index >> *child_shift;
            if child == children.len() {
                children.push(single_path(*child_shift, page));
            } else {
                push_page(&mut children[child], index & child_mask(*child_shift), page);
            }
        }
    }
}

fn pop_page<T>(node: &mut Arc<Node<T>>) -> bool {
    match Arc::make_mut(node) {
        Node::Leaf(pages) => {
            pages.pop();
            pages.is_empty()
        }
        Node::Branch { children, .. } => {
            if pop_page(children.last_mut().expect("nonempty directory")) {
                children.pop();
            }
            children.is_empty()
        }
    }
}

pub(super) struct Pages<'a, T> {
    directory: &'a PageDirectory<T>,
    indices: Range<usize>,
    current: std::slice::Iter<'a, Arc<Vec<T>>>,
}

impl<'a, T> Iterator for Pages<'a, T> {
    type Item = &'a Arc<Vec<T>>;
    fn next(&mut self) -> Option<Self::Item> {
        let index = self.indices.next()?;
        if self.current.as_slice().is_empty() {
            self.current = self.directory.leaf_suffix(index).iter();
        }
        self.current.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.indices.size_hint()
    }
}

impl<T> ExactSizeIterator for Pages<'_, T> {}

#[cfg(test)]
#[path = "page_directory_tests.rs"]
mod tests;
