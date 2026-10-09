//! Element List (Ctrl+Shift+E): every semantic element in the document,
//! filterable by category and text. Port of prism/gui/element_list_dialog.py.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use winsafe::prelude::*;

use crate::events;
use crate::gui::dialog::DialogBuilder;
use crate::navigation::element_nav::DocElement;
use crate::structured_text::elements::{ElementType, NAVIGABLE_CATEGORIES};

/// Indices into `elements` of those in `types` (all when `None`) whose text
/// contains `filter`, case-insensitively.
fn filter_elements(elements: &[DocElement], types: Option<&[ElementType]>, filter: &str) -> Vec<usize> {
    let filter = filter.to_lowercase();
    elements
        .iter()
        .enumerate()
        .filter(|(_, de)| types.is_none_or(|t| t.contains(&de.element.element_type)))
        .filter(|(_, de)| de.element.text.to_lowercase().contains(&filter))
        .map(|(i, _)| i)
        .collect()
}

/// Shows the list; the chosen element when the user presses OK or Enter on an item.
pub fn show(parent: &impl GuiParent, elements: Vec<DocElement>) -> Option<DocElement> {
    let names: Vec<&str> = NAVIGABLE_CATEGORIES.iter().map(|(n, _)| *n).collect();
    let d = DialogBuilder::new("Element List", 560);
    let category = d.combo("&Element type:", &names, Some(0));
    let filter = d.edit("&Filter:", "");
    let list = d.list_view("E&lements:", &[("Text", 430), ("Page", 70)], 260);
    d.ok_cancel("OK");

    let shown: Rc<RefCell<Vec<usize>>> = Rc::default();
    let chosen: Rc<Cell<Option<usize>>> = Rc::default();
    let elements = Rc::new(elements);

    let refresh = {
        let (category, filter, list, shown, elements) = (category.clone(), filter.clone(), list.clone(), shown.clone(), elements.clone());
        move || {
            let types = category.items().selected_index().and_then(|i| NAVIGABLE_CATEGORIES[i as usize].1);
            let idx = filter_elements(&elements, types, &filter.text().unwrap_or_default());
            let _ = list.items().delete_all();
            for &i in &idx {
                let de = &elements[i];
                let text: String = de.element.text.chars().take(80).collect();
                let _ = list.items().add(&[format!("{}: {text}", de.element.element_type.label()), (de.page_index + 1).to_string()], None, ());
            }
            events::announce(format!("{} elements", idx.len()));
            *shown.borrow_mut() = idx;
        }
    };
    let selected = {
        let (list, shown) = (list.clone(), shown.clone());
        move || list.items().iter_selected().next().and_then(|item| shown.borrow().get(item.index() as usize).copied())
    };

    let r = refresh.clone();
    category.on().cbn_sel_change(move || {
        r();
        Ok(())
    });
    let r = refresh.clone();
    filter.on().en_change(move || {
        r();
        Ok(())
    });
    d.wnd.on().wm_show_window(move |p| {
        if p.being_shown {
            refresh();
        }
        Ok(())
    });
    let (c, s, wnd) = (chosen.clone(), selected.clone(), d.wnd.clone());
    list.on().lvn_item_activate(move |_| {
        c.set(s());
        wnd.close();
        Ok(())
    });
    let c = chosen.clone();
    d.on_ok(move || {
        c.set(selected());
        true
    });
    d.run(parent);
    chosen.get().map(|i| elements[i].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structured_text::elements::{ElementMeta, HEADING_TYPES, SemanticElement};

    fn el(t: ElementType, text: &str, page: usize) -> DocElement {
        DocElement { page_index: page, element: SemanticElement { element_type: t, start_pos: 0, end_pos: 1, text: text.into(), meta: ElementMeta::default() } }
    }

    #[test]
    fn filters_by_category_and_text() {
        let els = vec![el(ElementType::Heading2, "Intro", 0), el(ElementType::Link, "intro link", 1), el(ElementType::Heading1, "Usage", 2)];
        assert_eq!(filter_elements(&els, Some(&HEADING_TYPES), ""), vec![0, 2]);
        assert_eq!(filter_elements(&els, None, "INTRO"), vec![0, 1]);
        assert_eq!(filter_elements(&els, Some(&[ElementType::Link]), "usage"), Vec::<usize>::new());
    }
}
