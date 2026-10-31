//! BASidekick's look: Ink, Paper and the hi-vis accent from
//! `@basidekick/brand`, Archivo Black for display and JetBrains Mono for
//! values, and the wordmark with its trace S.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{
    App, Div, FontWeight, Hsla, IntoElement, ParentElement, Styled, div, hsla, px, rgb, svg,
};
use std::borrow::Cow;

pub const DISPLAY_FONT: &str = "Archivo Black";
pub const MONO_FONT: &str = "JetBrains Mono";
pub const TRACE_S: &str = "brand/trace-s.svg";

fn hex(v: u32) -> Hsla {
    rgb(v).into()
}

pub fn ink() -> Hsla {
    hex(0x0e1013)
}
pub fn paper() -> Hsla {
    hex(0xf3f4ef)
}
pub fn accent() -> Hsla {
    hex(0xd4ff3a)
}
pub fn panel() -> Hsla {
    hex(0x171a1f)
}
pub fn panel_2() -> Hsla {
    hex(0x1c2026)
}
pub fn well() -> Hsla {
    hex(0x0b0d10)
}
pub fn ink_2() -> Hsla {
    hex(0xc9cdd3)
}
pub fn ink_3() -> Hsla {
    hex(0x9ba2ac)
}
pub fn line() -> Hsla {
    hsla(0.0, 0.0, 1.0, 0.08)
}
pub fn line_2() -> Hsla {
    hsla(0.0, 0.0, 1.0, 0.14)
}
pub fn ok() -> Hsla {
    hex(0x3fcf7f)
}
pub fn fault() -> Hsla {
    hex(0xf0a000)
}
pub fn alarm() -> Hsla {
    hex(0xe5484d)
}
/// Manual / BAS command: the brand's "hand" blue.
pub fn hand() -> Hsla {
    hex(0x3b63f7)
}
pub fn cold() -> Hsla {
    hex(0x2f62ff)
}
pub fn warm() -> Hsla {
    hex(0xff6a1f)
}

pub fn fonts(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(include_bytes!(
            "../../../assets/fonts/ArchivoBlack-Regular.ttf"
        )),
        Cow::Borrowed(include_bytes!(
            "../../../assets/fonts/JetBrainsMono-Regular.ttf"
        )),
        Cow::Borrowed(include_bytes!(
            "../../../assets/fonts/JetBrainsMono-Bold.ttf"
        )),
    ];
    let _ = cx.text_system().add_fonts(fonts);
}

/// gpui-component's dark theme, recolored to the brand.
pub fn apply_theme(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    let t = Theme::global_mut(cx);
    t.mono_font_family = MONO_FONT.into();
    t.radius = px(6.);
    t.radius_lg = px(10.);
    let c = &mut t.colors;
    c.background = ink();
    c.foreground = paper();
    c.border = line();
    c.input = line_2();
    c.ring = accent();
    c.muted = panel();
    c.muted_foreground = ink_3();
    c.secondary = panel_2();
    c.secondary_foreground = paper();
    c.secondary_hover = hex(0x232830);
    c.secondary_active = hex(0x2a3038);
    c.accent = panel_2();
    c.accent_foreground = paper();
    c.popover = panel();
    c.popover_foreground = paper();
    c.primary = accent();
    c.primary_foreground = ink();
    c.primary_hover = hex(0xdfff6b);
    c.primary_active = hex(0xc2ef20);
    c.button_primary = accent();
    c.button_primary_foreground = ink();
    c.button_primary_hover = hex(0xdfff6b);
    c.button_primary_active = hex(0xc2ef20);
    c.success = ok();
    c.warning = fault();
    c.danger = alarm();
    c.button_danger = alarm();
    c.button_danger_foreground = paper();
    c.info = hand();
    c.progress_bar = accent();
    c.selection = hsla(73.0 / 360.0, 1.0, 0.61, 0.25);
    c.scrollbar_thumb = line_2();
    c.list_hover = panel_2();
}

/// "BA", the trace S on its hi-vis chip, "idekick".
pub fn wordmark(size: f32) -> impl IntoElement {
    let letters = |s: &'static str| {
        div()
            .font_family(DISPLAY_FONT)
            .text_size(px(size))
            .line_height(px(size))
            .child(s)
    };
    div()
        .flex()
        .items_center()
        .text_color(paper())
        .child(letters("BA"))
        .child(
            div()
                .mx(px(size * 0.06))
                .size(px(size * 1.12))
                .rounded(px(size * 0.24))
                .bg(accent())
                .flex()
                .items_center()
                .justify_center()
                .child(svg().path(TRACE_S).size(px(size * 0.86)).text_color(ink())),
        )
        .child(letters("idekick"))
}

/// A small rounded label.
pub fn chip(text: impl Into<gpui_kit::SharedString>, fg: Hsla, bg: Hsla) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(4.))
        .bg(bg)
        .text_color(fg)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

/// The same color at a given opacity, for tinted backgrounds.
pub fn tint(c: Hsla, a: f32) -> Hsla {
    Hsla { a, ..c }
}
