//! Fixture for `tests/ai_semantic_rust_test.rs`. Line numbers are pinned by
//! that test: `Widget::label` must start on line 10, `make_widget` on 15.

pub struct Widget {
    name: String,
}

impl Widget {
    // Reads the widget's label.
    fn label(&self) -> &str {
        &self.name
    }
}

pub fn make_widget(name: &str) -> Widget {
    Widget {
        name: name.to_string(),
    }
}

fn handle() {}

mod nested {
    pub fn handle() {}
}
