//! Table-of-contents sidebar: a tree of the document's TOC, or of its pages
//! when it has none. Port of prism/gui/toc_tree.py.

use std::cell::RefCell;

use winsafe::{self as w, co, gui, msg, prelude::*};

use crate::document::TocEntry;
use crate::platform::win32::a11y;

pub struct TocTree {
    pub tree: gui::TreeView,
    /// Every item that leads to a page, in tree order: (HTREEITEM, page index).
    items: RefCell<Vec<(isize, usize)>>,
}

/// The item to highlight for `page`: the selected item when it is already on
/// that page (two entries can share a page), else the last item starting at
/// or before it.
fn item_for_page(items: &[(isize, usize)], selected: Option<isize>, page: usize) -> Option<isize> {
    if let Some(sel) = selected
        && items.iter().any(|&(h, p)| h == sel && p == page)
    {
        return Some(sel);
    }
    items.iter().filter(|&&(_, p)| p <= page).last().map(|&(h, _)| h)
}

impl TocTree {
    pub fn new(parent: &gui::WindowMain) -> Self {
        let tree = gui::TreeView::new(parent, gui::TreeViewOpts {
            control_style: co::TVS::HASBUTTONS | co::TVS::HASLINES | co::TVS::LINESATROOT | co::TVS::SHOWSELALWAYS,
            ..Default::default()
        });
        Self { tree, items: RefCell::default() }
    }

    pub fn name_control(&self) {
        a11y::set_name(self.tree.hwnd().ptr() as isize, "Table of contents");
    }

    /// Rebuilds the tree: a `title` node holding the TOC when `use_toc`, else one item per page.
    pub fn populate(&self, title: &str, toc: &[TocEntry], use_toc: bool, page_count: usize) {
        self.clear();
        let Ok(root) = self.tree.items().add_root(title, None, ()) else { return };
        let mut items = Vec::new();
        if use_toc {
            fn add(parent: &gui::TreeViewItem<'_, ()>, entries: &[TocEntry], items: &mut Vec<(isize, usize)>) {
                for e in entries {
                    if let Ok(item) = parent.add_child(&e.title, None, ()) {
                        items.push((item.htreeitem().ptr() as isize, e.page_index));
                        add(&item, &e.children, items);
                        let _ = item.expand(true);
                    }
                }
            }
            add(&root, toc, &mut items);
        } else {
            for i in 0..page_count {
                if let Ok(item) = root.add_child(&format!("Page {}", i + 1), None, ()) {
                    items.push((item.htreeitem().ptr() as isize, i));
                }
            }
        }
        let _ = root.expand(true);
        *self.items.borrow_mut() = items;
        self.select_page(0);
    }

    pub fn clear(&self) {
        self.items.borrow_mut().clear();
        let _ = self.tree.items().delete_all();
    }

    /// The page an item leads to.
    pub fn page_of(&self, hitem: isize) -> Option<usize> {
        self.items.borrow().iter().find(|&&(h, _)| h == hitem).map(|&(_, p)| p)
    }

    pub fn selected_page(&self) -> Option<usize> {
        self.page_of(self.selected()?)
    }

    fn selected(&self) -> Option<isize> {
        self.tree.items().iter_selected().next().map(|i| i.htreeitem().ptr() as isize)
    }

    /// Highlights the entry for `page` without moving to it (a programmatic
    /// selection reports `TVC_UNKNOWN`, which the main window ignores).
    pub fn select_page(&self, page: usize) {
        let Some(h) = item_for_page(&self.items.borrow(), self.selected(), page) else { return };
        let hitem = unsafe { w::HTREEITEM::from_ptr(h as _) };
        unsafe {
            let _ = self.tree.hwnd().SendMessage(msg::TvmSelectItem { action: co::TVGN::CARET, hitem: &hitem });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::item_for_page;

    #[test]
    fn highlights_the_section_holding_the_page() {
        let items = [(10, 0), (11, 3), (12, 3), (13, 7)];
        assert_eq!(item_for_page(&items, None, 5), Some(12));
        assert_eq!(item_for_page(&items, Some(11), 3), Some(11)); // keep the chosen entry of a shared page
        assert_eq!(item_for_page(&items, Some(10), 8), Some(13));
        assert_eq!(item_for_page(&[(10, 2)], None, 1), None);
    }
}
