use fontdue::{Font, FontSettings};
use std::sync::OnceLock;

static FONT: OnceLock<Font> = OnceLock::new();

pub fn get_font() -> &'static Font {
    FONT.get_or_init(|| {
        let font_bytes = include_bytes!("font.ttf") as &[u8];
        Font::from_bytes(font_bytes, FontSettings::default()).unwrap()
    })
}
