//! Minimal assets retained after the retired Next.js embed.
//!
//! RC-20 stopped `build.rs` from exporting `web/out/` and rust-embed no
//! longer ships the Code UI bytes. The loopback security notice is still a
//! product contract, so keep its two small, static HTML pages available to
//! the existing fallback handler without rebuilding or embedding the UI.

use std::borrow::Cow;

/// Former rust-embed payload. Kept so existing `content.data` reads compile.
pub struct EmbeddedFile {
    pub data: Cow<'static, [u8]>,
}

pub struct WebAssets;

impl WebAssets {
    pub fn get(path: &str) -> Option<EmbeddedFile> {
        let data = match path {
            "remote-notice/index.html" => include_str!("../../web/public/remote-notice/index.html"),
            "remote-notice/zh-CN/index.html" => {
                include_str!("../../web/public/remote-notice/zh-CN/index.html")
            }
            _ => return None,
        };
        Some(EmbeddedFile {
            data: Cow::Borrowed(data.as_bytes()),
        })
    }
}
