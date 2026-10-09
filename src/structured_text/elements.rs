//! Semantic element types for structured document navigation.
//!
//! Positions are **char indices** (Unicode scalar values) into the page text,
//! matching Python string indices in Prism. The GUI converts them to RichEdit
//! offsets (UTF-16 units, one unit per line break) at the control boundary.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ElementType {
    Heading1,
    Heading2,
    Heading3,
    Heading4,
    Heading5,
    Heading6,
    Link,
    Table,
    List,
    ListItem,
    Quote,
    Figure,
    CodeBlock,
    Paragraph,
}

impl ElementType {
    pub fn heading(level: u8) -> Self {
        match level {
            1 => Self::Heading1,
            2 => Self::Heading2,
            3 => Self::Heading3,
            4 => Self::Heading4,
            5 => Self::Heading5,
            _ => Self::Heading6,
        }
    }

    /// `Some(1..=6)` for headings.
    pub fn heading_level(self) -> Option<u8> {
        match self {
            Self::Heading1 => Some(1),
            Self::Heading2 => Some(2),
            Self::Heading3 => Some(3),
            Self::Heading4 => Some(4),
            Self::Heading5 => Some(5),
            Self::Heading6 => Some(6),
            _ => None,
        }
    }

    pub fn is_heading(self) -> bool {
        self.heading_level().is_some()
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Heading1 => "Heading 1",
            Self::Heading2 => "Heading 2",
            Self::Heading3 => "Heading 3",
            Self::Heading4 => "Heading 4",
            Self::Heading5 => "Heading 5",
            Self::Heading6 => "Heading 6",
            Self::Link => "Link",
            Self::Table => "Table",
            Self::List => "List",
            Self::ListItem => "List item",
            Self::Quote => "Blockquote",
            Self::Figure => "Figure",
            Self::CodeBlock => "Code block",
            Self::Paragraph => "Paragraph",
        }
    }
}

pub const HEADING_TYPES: [ElementType; 6] = [
    ElementType::Heading1,
    ElementType::Heading2,
    ElementType::Heading3,
    ElementType::Heading4,
    ElementType::Heading5,
    ElementType::Heading6,
];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ElementMeta {
    /// Heading level 1-6.
    pub level: Option<u8>,
    /// Link target.
    pub url: Option<String>,
    /// Image alt text.
    pub alt: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticElement {
    pub element_type: ElementType,
    /// Char index of the element's first character in the page text.
    pub start_pos: usize,
    /// Char index one past the element's last character.
    pub end_pos: usize,
    pub text: String,
    pub meta: ElementMeta,
}

/// The categories offered in the Element List dialog; `None` means all elements.
pub const NAVIGABLE_CATEGORIES: [(&str, Option<&[ElementType]>); 8] = [
    ("Headings", Some(&HEADING_TYPES)),
    ("Links", Some(&[ElementType::Link])),
    ("Tables", Some(&[ElementType::Table])),
    ("Lists", Some(&[ElementType::List])),
    ("Blockquotes", Some(&[ElementType::Quote])),
    ("Figures", Some(&[ElementType::Figure])),
    ("Code blocks", Some(&[ElementType::CodeBlock])),
    ("All elements", None),
];
