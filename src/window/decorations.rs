mod font;
mod hit;
mod paint;

use std::sync::Arc;

use ab_glyph::FontArc;
use orbit_config::{Button, DecorationConfig};
use smithay::{
    backend::renderer::{
        ImportMem, Renderer,
        element::{
            Kind,
            memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            solid::{SolidColorBuffer, SolidColorRenderElement},
        },
    },
    desktop::Window,
    reexports::wayland_protocols::xdg::{
        decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
        shell::server::xdg_toplevel::State as XdgState,
    },
    utils::{Logical, Physical, Point, Rectangle, Scale, Size},
    wayland::shell::xdg::{ToplevelSurface, decoration::XdgDecorationHandler},
};

use crate::{
    protocols::foreign_toplevel::with_title_and_app_id,
    state::Orbit,
    window::{self, configure, with_window_state},
    workspaces::window_id,
};

use self::paint::{BarKey, bar_pixels, draw_bar};

pub use self::{
    font::{FontLoader, LoadedFont},
    hit::{DecorationHit, hit_test},
};

const OUTLINE_WIDTH: i32 = 3;
const URGENT_COLOR: [f32; 4] = [0.96, 0.55, 0.15, 1.0];

/// A render element produced for a window's decorations.
#[expect(
    clippy::large_enum_variant,
    reason = "short-lived as each one is moved into the frame's element list right away"
)]
pub enum DecorationElement<R: Renderer> {
    /// Rasterized title bar.
    Bar(MemoryRenderBufferRenderElement<R>),
    /// Solid-color urgency outline segment.
    Solid(SolidColorRenderElement),
}

/// Shared decoration configuration and its resolved title-bar font.
#[derive(Debug, Default)]
pub struct DecorationStyle {
    /// Title-bar geometry, controls, typography, and colors.
    pub config: DecorationConfig,
    /// Loaded font used for titles, if available.
    font: Option<FontArc>,
}

impl DecorationStyle {
    /// Creates a decoration style from configuration and an optional font.
    pub fn new(config: DecorationConfig, font: Option<FontArc>) -> Self {
        Self { config, font }
    }

    /// Returns the configured title-bar height in logical pixels.
    pub fn height(&self) -> i32 {
        self.config.height
    }

    /// Returns the loaded title font, if one is available.
    pub fn font(&self) -> Option<&FontArc> {
        self.font.as_ref()
    }

    /// Returns whether `config` requires loading a different title font.
    ///
    /// Geometry, colors, and button settings do not affect this result.
    pub fn font_differs(&self, config: &DecorationConfig) -> bool {
        self.config.title != config.title
            || self.config.font != config.font
            || self.config.font_weight != config.font_weight
    }
}

/// Per-window decoration state and cached render buffers.
#[derive(Debug, Default)]
pub struct Decoration {
    /// Cached title bars for up to two output scales.
    bars: Vec<(BarKey, MemoryRenderBuffer)>,
    /// Reusable buffers for the four sides of an urgency outline.
    outline: [SolidColorBuffer; 4],
    /// Button currently under the pointer, if any.
    pub hovered: Option<Button>,
}

impl Decoration {
    /// Discards cached title bars so they are redrawn with a new style.
    pub fn clear_cache(&mut self) {
        self.bars.clear();
    }
}

impl Orbit {
    /// Applies a completed font load if it is still the latest request.
    fn title_font_loaded(&mut self, loaded: LoadedFont) {
        if loaded.request != self.font_loader.latest {
            return;
        }
        let config = self.decoration_style.config.clone();
        self.set_decoration_style(Arc::new(DecorationStyle::new(config, loaded.font)));
        self.queue_redraw_all();
    }

    /// Applies `style` to every window and clears their bar caches.
    ///
    /// Callers must relayout windows when the style's bar height changes.
    pub fn set_decoration_style(&mut self, style: Arc<DecorationStyle>) {
        self.decoration_style = style.clone();
        for window in self.windows.values() {
            with_window_state(window, |state| {
                state.style = style.clone();
                state.decoration.clear_cache();
            });
        }
    }

    /// Applies a client/server decoration mode and reconfigures the toplevel.
    ///
    /// Maximized and tiled windows are rearranged because a title bar changes
    /// the content area available to them.
    fn set_decoration_mode(&mut self, toplevel: &ToplevelSurface, mode: Mode) {
        toplevel.with_pending_state(|pending| pending.decoration_mode = Some(mode));
        if let Some(window) = self.window_for_surface(toplevel.wl_surface()) {
            self.arrange_window(&window);
        }
        configure(toplevel);
    }

    /// Updates the hovered title-bar button and redraws affected windows.
    ///
    /// Non-button hits and `None` clear the current hover state.
    pub fn set_decoration_hover(&mut self, hover: Option<(&Window, DecorationHit)>) {
        let hover = hover.and_then(|(window, hit)| Some((window_id(window)?, hit.button()?)));
        if hover == self.input.decoration_hover {
            return;
        }
        let old = self.input.decoration_hover.take();
        for (id, button) in [
            old.map(|(id, _)| (id, None)),
            hover.map(|(id, b)| (id, Some(b))),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(window) = self.windows.get(&id).cloned() {
                with_window_state(&window, |state| state.decoration.hovered = button);
                self.queue_redraw_window(&window);
            }
        }
        self.input.decoration_hover = hover;
    }
}

impl XdgDecorationHandler for Orbit {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        // Sent with the next configure; clients draw their own by default.
        toplevel.with_pending_state(|pending| pending.decoration_mode = Some(Mode::ClientSide));
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        self.set_decoration_mode(&toplevel, mode);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.set_decoration_mode(&toplevel, Mode::ClientSide);
    }
}

/// Pushes render elements for a window's decorations and urgency outline.
///
/// `frame` is output-local logical geometry; `offset` is a physical-pixel
/// translation applied consistently with the window's surfaces. Elements are
/// pushed topmost first.
pub fn render_elements<R>(
    renderer: &mut R,
    window: &Window,
    frame: Rectangle<i32, Logical>,
    offset: Point<i32, Physical>,
    scale: Scale<f64>,
    alpha: f32,
    mut push: impl FnMut(DecorationElement<R>),
) where
    R: Renderer + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    let ssd = window::uses_server_side_decorations(window);
    let style = with_window_state(window, |state| state.style.clone());
    let titled = ssd && style.config.title && style.font.is_some();
    let active = is_activated(window);

    with_window_state(window, |state| {
        let to_physical = |loc: Point<i32, Logical>| loc.to_physical_precise_round(scale) + offset;

        if ssd {
            let deco = &mut state.decoration;
            let (width, scale_bits, hovered) = (frame.size.w, scale.x.to_bits(), deco.hovered);
            // The title is compared where it is, and only copied for a
            // bar that has to be drawn.
            let cached = deco.bars.iter().position(|(k, _)| {
                k.width == width
                    && k.scale == scale_bits
                    && k.active == active
                    && k.hovered == hovered
                    && title_is(window, titled, &k.title)
            });
            let index = match cached {
                Some(index) => Some(index),
                None => {
                    let key = BarKey {
                        width,
                        scale: scale_bits,
                        active,
                        hovered,
                        title: bar_title(window, titled),
                    };
                    draw_bar(&style, &key, scale.x).map(|buffer| {
                        if deco.bars.len() >= 2 {
                            deco.bars.remove(0);
                        }
                        deco.bars.push((key, buffer));
                        deco.bars.len() - 1
                    })
                }
            };
            if let Some(index) = index {
                let buffer = &deco.bars[index].1;
                let size: Size<i32, Logical> = (frame.size.w, style.height()).into();
                let pixels = bar_pixels(size, scale.x);
                let loc = to_physical(frame.loc).to_f64();
                match MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    loc,
                    buffer,
                    Some(alpha),
                    Some(Rectangle::from_size(
                        (f64::from(pixels.0), f64::from(pixels.1)).into(),
                    )),
                    Some(size),
                    Kind::Unspecified,
                ) {
                    Ok(element) => push(DecorationElement::Bar(element)),
                    Err(err) => tracing::warn!(?err, "failed to upload a title bar"),
                }
            }
        }

        if state.urgent {
            let w = OUTLINE_WIDTH;
            let outer = Rectangle::new(
                frame.loc - Point::from((w, w)),
                (frame.size.w + 2 * w, frame.size.h + 2 * w).into(),
            );
            let sides = [
                Rectangle::new(outer.loc, (outer.size.w, w).into()),
                Rectangle::new(
                    (outer.loc.x, outer.loc.y + outer.size.h - w).into(),
                    (outer.size.w, w).into(),
                ),
                Rectangle::new(
                    (outer.loc.x, outer.loc.y + w).into(),
                    (w, frame.size.h).into(),
                ),
                Rectangle::new(
                    (outer.loc.x + outer.size.w - w, outer.loc.y + w).into(),
                    (w, frame.size.h).into(),
                ),
            ];
            for (buffer, rect) in state.decoration.outline.iter_mut().zip(sides) {
                buffer.update(rect.size, URGENT_COLOR);
                push(DecorationElement::Solid(
                    SolidColorRenderElement::from_buffer(
                        buffer,
                        to_physical(rect.loc),
                        scale,
                        alpha,
                        Kind::Unspecified,
                    ),
                ));
            }
        }
    })
}

/// Returns whether `window` is currently activated.
fn is_activated(window: &Window) -> bool {
    window.toplevel().is_some_and(|t| {
        t.with_committed_state(|state| {
            state.is_some_and(|s| s.states.contains(XdgState::Activated))
        })
    })
}

/// Returns the window title when titled decorations are enabled.
fn bar_title(window: &Window, titled: bool) -> String {
    if titled {
        with_title_and_app_id(window, |title, _| title.to_owned()).unwrap_or_default()
    } else {
        String::new()
    }
}

/// Checks a window title without allocating a new `String`.
fn title_is(window: &Window, titled: bool, title: &str) -> bool {
    if titled {
        with_title_and_app_id(window, |current, _| current == title).unwrap_or(title.is_empty())
    } else {
        title.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_new_font_needs_loading() {
        let style = DecorationStyle::default();
        let mut config = style.config.clone();
        config.height += 10;
        config.colors.active = [1.0, 0.0, 0.0, 1.0];
        assert!(!style.font_differs(&config));
        config.font_weight = Some(700.0);
        assert!(style.font_differs(&config));
        let mut config = style.config.clone();
        config.title = !config.title;
        assert!(style.font_differs(&config));
    }
}
