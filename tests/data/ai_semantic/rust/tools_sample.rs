//! Fixture for `tests/ai_semantic_tools_test.rs` (semantic tool handlers).
//!
//! Kept separate from `sample.rs`, whose line numbers are pinned by
//! `tests/ai_semantic_rust_test.rs` — this file's contents matter
//! (Widget::new + its call site, an ambiguous `handle`), not its line
//! numbers.

pub struct Widget {
    name: String,
}

impl Widget {
    pub fn new(name: String) -> Self {
        Widget { name }
    }

    // Reads the widget's label.
    fn label(&self) -> &str {
        &self.name
    }
}

pub fn make_widget(name: &str) -> Widget {
    let name = name.to_string();
    Widget::new(name)
}

fn handle() {}

mod nested {
    pub fn handle() {}
}
