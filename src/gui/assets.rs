//! Embedded brand assets, with Ely's fonts and icons as the fallback.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if path == "libra/logo.svg" {
            return Ok(Some(Cow::Borrowed(include_bytes!("assets/libra-logo.svg"))));
        }
        ely_gpui_component::Assets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut files = ely_gpui_component::Assets.list(path)?;
        if "libra/logo.svg".starts_with(path) {
            files.push("libra/logo.svg".into());
        }
        Ok(files)
    }
}
