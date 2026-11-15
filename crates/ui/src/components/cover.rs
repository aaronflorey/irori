use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Background, ElementId, Hsla, Img, IntoElement, ObjectFit, ParentElement,
    Pixels, Rems, RenderOnce, SharedString, Styled, StyledImage as _, Window, div, img,
    linear_color_stop, linear_gradient, rems, rgb,
};
use irori_core::GameInfo;

use crate::art::{Art, art_store};
use crate::models::models;
use crate::motion::fade_in;

/// What there is to show for a piece of art right now.
// Built and consumed within one render, so the size of `Img` costs nothing.
#[allow(clippy::large_enum_variant)]
pub enum ArtImage {
    Ready(Img),
    /// Being downloaded or thumbnailed; observers of the art store re-render.
    Loading,
    /// No URL, or the download or decode failed.
    Missing,
}

/// Thumbnail of `url` at `width` physical pixels, starting its production when
/// it is not on disk yet. `keep_alpha` for logos.
pub fn art_image(url: &str, width: u32, keep_alpha: bool, cx: &mut App) -> ArtImage {
    match art_store(cx).update(cx, |s, cx| s.get(url, width, keep_alpha, cx)) {
        Art::Ready(path) => ArtImage::Ready(img(path)),
        Art::Loading => ArtImage::Loading,
        Art::Missing => ArtImage::Missing,
    }
}

/// Whether the thumbnail for these arguments arrived moments ago, so it
/// should fade in rather than pop.
pub fn art_is_fresh(url: &str, width: u32, keep_alpha: bool, cx: &mut App) -> bool {
    art_store(cx).update(cx, |s, _| s.is_fresh(url, width, keep_alpha))
}

/// Art that fills its parent, fading in under `id` when it just arrived.
pub fn art_arrival(id: impl Into<ElementId>, fresh: bool, art: impl IntoElement) -> AnyElement {
    if fresh {
        fade_in(id, div().size_full().child(art))
    } else {
        art.into_any_element()
    }
}

/// Thumbnail of `url` at `width` physical pixels if it is ready, starting its
/// production otherwise. `keep_alpha` for logos.
pub fn art_img(url: &str, width: u32, keep_alpha: bool, cx: &mut App) -> Option<Img> {
    match art_image(url, width, keep_alpha, cx) {
        ArtImage::Ready(img) => Some(img),
        ArtImage::Loading | ArtImage::Missing => None,
    }
}

/// Fills its parent while art is on its way: a softly pulsing surface (still
/// when the user reduces motion).
pub fn art_loading() -> AnyElement {
    Skeleton::new().size_full().into_any_element()
}

/// Background for art that does not exist: a quiet diagonal wash, so a game
/// without art reads as designed rather than broken.
pub fn missing_art_fill(cx: &App) -> Background {
    let theme = cx.theme();
    linear_gradient(
        160.,
        linear_color_stop(theme.border, 0.),
        linear_color_stop(theme.muted, 0.7),
    )
}

/// Fills its parent when a game has no art of this kind: the wash with a
/// centred gamepad. Callers that do not show the title nearby use [`GameCover`].
pub fn art_missing(cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .bg(missing_art_fill(cx))
        .child(
            Icon::new(IconName::Gamepad2)
                .large()
                .text_color(theme.muted_foreground),
        )
        .into_any_element()
}

/// A `#rrggbb` colour of the systems catalog.
fn catalog_color(hex: &str) -> Option<Hsla> {
    let v = u32::from_str_radix(hex.strip_prefix('#')?, 16).ok()?;
    Some(rgb(v).into())
}

/// The generated cover of an emulated game without box art: the system's
/// colours, its short name and the title. Like box art, it carries its own
/// colours in every theme, with text that stays legible on them.
fn system_cover(system: &str, title: &SharedString, small: bool) -> Option<AnyElement> {
    let sys = irori_engines::systems::get(system)?;
    let (base, accent) = (
        catalog_color(&sys.colors[0])?,
        catalog_color(&sys.colors[1])?,
    );
    let text = if base.l > 0.6 {
        gpui_kit::black()
    } else {
        gpui_kit::white()
    };
    Some(
        v_flex()
            .size_full()
            .justify_between()
            .p_3()
            .bg(linear_gradient(
                160.,
                linear_color_stop(base, 0.55),
                linear_color_stop(accent, 1.),
            ))
            .text_color(text)
            .child(
                div()
                    .text_xs()
                    .font_bold()
                    .opacity(0.85)
                    .child(sys.short.clone()),
            )
            .child(
                div()
                    .when_else(small, |d| d.text_sm(), |d| d.text_lg())
                    .font_bold()
                    .line_clamp(4)
                    .child(title.clone()),
            )
            .into_any_element(),
    )
}

/// Physical pixel width of a length in rems in this window.
pub fn physical_width(width: Rems, window: &Window) -> u32 {
    let logical: f32 = (window.rem_size() * width.0).into();
    (logical * window.scale_factor()).ceil() as u32
}

/// Covers narrower than this (rows, sidebar, download lists) have no room for
/// the emulator's logo.
const ENGINE_LOGO_MIN_REM: f32 = 7.;

/// The logo of the emulator a ROM game plays with, as a chip in a corner of
/// the cover: at the bottom of box art, at the top of a cover Irori draws
/// itself, where the title sits at the foot.
fn engine_logo(
    url: &str,
    cover_width: Rems,
    top: bool,
    window: &Window,
    cx: &mut App,
) -> Option<AnyElement> {
    let size = rems((cover_width.0 * 0.2).clamp(1.5, 2.5));
    let logo = art_img(url, physical_width(size, window), true, cx)?;
    Some(
        div()
            .absolute()
            .right_2()
            .when_else(top, |d| d.top_2(), |d| d.bottom_2())
            .size(size)
            .p_1()
            .rounded_md()
            .bg(cx.theme().background.opacity(0.8))
            .child(logo.size_full().object_fit(ObjectFit::Contain))
            .into_any_element(),
    )
}

/// A 2:3 cover tile. Pulses while its art loads; a game without cover art
/// gets its title on a quiet wash instead.
#[derive(IntoElement)]
pub struct GameCover {
    game: Arc<GameInfo>,
    width: Rems,
    radius: Option<Pixels>,
}

impl GameCover {
    pub fn new(game: Arc<GameInfo>) -> Self {
        Self {
            game,
            width: rems(11.),
            radius: None,
        }
    }

    /// Tile width; height follows the 2:3 cover ratio.
    pub fn width(mut self, width: Rems) -> Self {
        self.width = width;
        self
    }

    /// Corner radius; the theme's large radius by default. Small thumbnails
    /// want less.
    pub fn radius(mut self, radius: Pixels) -> Self {
        self.radius = Some(radius);
        self
    }
}

impl RenderOnce for GameCover {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let px = physical_width(self.width, window);
        let (image, fresh) = match self.game.cover_art() {
            Some(url) => (
                art_image(url, px, false, cx),
                art_is_fresh(url, px, false, cx),
            ),
            None => (ArtImage::Missing, false),
        };
        let engine_logo = (self.width.0 >= ENGINE_LOGO_MIN_REM)
            .then(|| {
                let url = models(cx)
                    .library
                    .read(cx)
                    .engine_icons
                    .get(&self.game.id())
                    .cloned()?;
                let top = !matches!(image, ArtImage::Ready(_));
                engine_logo(&url, self.width, top, window, cx)
            })
            .flatten();
        let theme = cx.theme();
        let radius = self.radius.unwrap_or(theme.radius_lg);
        let base = div()
            .relative()
            .w(self.width)
            .h(rems(self.width.0 * 1.5))
            .flex_shrink_0()
            .overflow_hidden()
            .rounded(radius)
            .bg(theme.muted);
        let tile = match image {
            // Images are clipped to rectangles, so they carry the corners themselves.
            ArtImage::Ready(image) => base.child(art_arrival(
                SharedString::from(format!("cover-in:{}", self.game.id().key())),
                fresh,
                image
                    .size_full()
                    .rounded(radius)
                    .object_fit(ObjectFit::Cover),
            )),
            ArtImage::Loading => base.child(art_loading()),
            // Tiny thumbnails (download rows, sidebar) have no room for a title.
            ArtImage::Missing if self.width.0 < 5. => base.child(art_missing(cx)),
            ArtImage::Missing
                if let Some(cover) = self.game.system.as_deref().and_then(|s| {
                    system_cover(s, &self.game.title.clone().into(), self.width.0 < 9.)
                }) =>
            {
                base.child(cover)
            }
            ArtImage::Missing => base.child(
                v_flex()
                    .size_full()
                    .justify_between()
                    .p_3()
                    .bg(missing_art_fill(cx))
                    .child(
                        Icon::new(IconName::Gamepad2)
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .when_else(self.width.0 < 9., |d| d.text_sm(), |d| d.text_lg())
                            .font_bold()
                            .line_clamp(4)
                            .text_color(theme.foreground)
                            .child(self.game.title.clone()),
                    ),
            ),
        };
        tile.children(engine_logo)
    }
}
