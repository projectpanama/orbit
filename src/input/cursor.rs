use std::{
    collections::HashMap,
    sync::{PoisonError, mpsc},
};

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData},
    output::Output,
    reexports::{
        calloop::{LoopHandle, channel},
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point, Transform},
    wayland::compositor::with_states,
};

use crate::state::Orbit;

/// Theme used when `XCURSOR_THEME` is unset.
const DEFAULT_THEME: &str = "default";
/// Fallback theme tried after the requested theme.
const FALLBACK_THEME: &str = "Adwaita";
/// Logical cursor size used when `XCURSOR_SIZE` is unset.
const DEFAULT_SIZE: u32 = 24;

/// Loaded for each output's scale as soon as the output appears: what
/// Orbit itself shows (moving, resizing) and what clients ask for most.
const COMMON_ICONS: [CursorIcon; 13] = [
    CursorIcon::Default,
    CursorIcon::Pointer,
    CursorIcon::Text,
    CursorIcon::Grabbing,
    CursorIcon::NResize,
    CursorIcon::SResize,
    CursorIcon::EResize,
    CursorIcon::WResize,
    CursorIcon::NeResize,
    CursorIcon::NwResize,
    CursorIcon::SeResize,
    CursorIcon::SwResize,
    CursorIcon::Wait,
];

/// A cursor image ready to render, including its pointer hotspot.
#[derive(Debug, Clone)]
pub struct CursorImage {
    /// Pixel buffer containing the cursor image.
    pub buffer: MemoryRenderBuffer,
    /// Hotspot position in logical pixels.
    pub hotspot: Point<i32, Logical>,
}

/// Result delivered by the cursor loader for one icon and scale.
pub struct LoadedCursor {
    /// Icon requested by the compositor.
    icon: CursorIcon,
    /// Output scale requested for the icon.
    scale: u32,
    /// Loaded image or `None` when no theme contains the icon.
    image: Option<CursorImage>,
}

/// State of one cursor entry in the asynchronous cache.
enum Slot {
    /// A request has been sent but no result has arrived.
    Loading,
    /// Loading completed, with an optional image.
    Loaded(Option<CursorImage>),
}

/// Asynchronously loaded named cursors used by the software cursor.
///
/// Theme lookup and image parsing run on a worker thread. Until an icon is
/// cached, [`Self::get`] requests it and falls back to the default arrow.
pub struct CursorTheme {
    /// Channel used to request icons from the loader thread.
    requests: mpsc::Sender<(CursorIcon, u32)>,
    /// Cached loading states keyed by icon and integer scale.
    cache: HashMap<(CursorIcon, u32), Slot>,
}

impl CursorTheme {
    /// Starts the loader thread and registers its result channel.
    pub fn new(handle: &LoopHandle<'static, Orbit>) -> anyhow::Result<Self> {
        let (results, receiver) = channel::channel();

        handle
            .insert_source(receiver, |event, _, state| {
                if let channel::Event::Msg(loaded) = event {
                    state.cursor_loaded(loaded);
                }
            })
            .map_err(|err| err.error)?;

        let (requests, pending) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("orbit-cursor".into())
            .spawn(move || Loader::load().serve(&pending, &results));

        // Without the thread every request fails, and the fallback square is drawn.
        if let Err(err) = spawned {
            tracing::warn!(%err, "can't start loading the cursor theme");
        }

        Ok(Self {
            requests,
            cache: HashMap::new(),
        })
    }

    /// Requests common compositor and client icons at `scale` ahead of use.
    pub fn preload(&mut self, scale: u32) {
        for icon in COMMON_ICONS {
            self.request(icon, scale);
        }
    }

    /// Returns the cached image for `icon` at `scale`.
    ///
    /// While loading, or when the theme lacks `icon`, the default arrow is
    /// used. Returns `None` only when that fallback is also unavailable.
    pub fn get(&mut self, icon: CursorIcon, scale: u32) -> Option<CursorImage> {
        if let Some(image) = self.cached(icon, scale) {
            return Some(image);
        }
        self.cached(CursorIcon::Default, scale)
    }

    /// Returns a completed cache entry or requests the icon when absent.
    fn cached(&mut self, icon: CursorIcon, scale: u32) -> Option<CursorImage> {
        match self.cache.get(&(icon, scale)) {
            Some(Slot::Loaded(image)) => image.clone(),
            Some(Slot::Loading) => None,
            None => {
                self.request(icon, scale);
                None
            }
        }
    }

    /// Queues an icon request once and records its loading state.
    fn request(&mut self, icon: CursorIcon, scale: u32) {
        if self.cache.contains_key(&(icon, scale)) {
            return;
        }
        // The thread only stops if it panicked or never started.
        if self.requests.send((icon, scale)).is_ok() {
            self.cache.insert((icon, scale), Slot::Loading);
        } else {
            self.cache.insert((icon, scale), Slot::Loaded(None));
        }
    }
}

impl Orbit {
    /// Enables software cursor rendering and preloads common icons.
    pub fn enable_software_cursor(&mut self) -> anyhow::Result<()> {
        let mut theme = CursorTheme::new(&self.loop_handle)?;
        for output in self.outputs.keys() {
            theme.preload(cursor_scale(output));
        }
        self.input.cursor_theme = Some(theme);
        Ok(())
    }

    /// Stores a loader result and redraws if a named cursor is visible.
    fn cursor_loaded(&mut self, loaded: LoadedCursor) {
        let Some(theme) = self.input.cursor_theme.as_mut() else {
            return;
        };
        theme
            .cache
            .insert((loaded.icon, loaded.scale), Slot::Loaded(loaded.image));
        // The pointer may be showing the arrow in its place.
        if matches!(self.input.cursor_status, CursorImageStatus::Named(_)) {
            self.queue_redraw_all();
        }
    }
}

/// Returns the integer buffer scale used for cursor images on `output`.
pub fn cursor_scale(output: &Output) -> u32 {
    output.current_scale().fractional_scale().ceil().max(1.0) as u32
}

/// Returns the hotspot set by a client cursor surface.
///
/// Returns the origin when the surface has no cursor metadata.
pub fn surface_hotspot(surface: &WlSurface) -> Point<i32, Logical> {
    with_states(surface, |states| {
        states
            .data_map
            .get::<CursorImageSurfaceData>()
            .map(|data| data.lock().unwrap_or_else(PoisonError::into_inner).hotspot)
            .unwrap_or_default()
    })
}

/// State owned by the cursor loader thread.
struct Loader {
    /// Themes searched in priority order.
    themes: Vec<xcursor::CursorTheme>,
    /// Desired logical cursor size.
    size: u32,
}

impl Loader {
    /// Loads themes and cursor size from the environment.
    fn load() -> Self {
        let name = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| DEFAULT_THEME.into());
        let size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|size| size.parse().ok())
            .unwrap_or(DEFAULT_SIZE);
        let themes = [name.as_str(), FALLBACK_THEME]
            .into_iter()
            .map(xcursor::CursorTheme::load)
            .collect();
        tracing::info!(theme = name, size, "loading cursor theme");
        Self { themes, size }
    }

    /// Serves icon requests until the request or result channel closes.
    fn serve(
        &self,
        requests: &mpsc::Receiver<(CursorIcon, u32)>,
        results: &channel::Sender<LoadedCursor>,
    ) {
        while let Ok((icon, scale)) = requests.recv() {
            let image = std::iter::once(icon.name())
                .chain(icon.alt_names().iter().copied())
                .find_map(|name| self.load_image(name, scale));
            if image.is_none() {
                tracing::debug!(icon = icon.name(), "cursor theme has no such cursor");
            }
            if results.send(LoadedCursor { icon, scale, image }).is_err() {
                return;
            }
        }
    }

    /// Loads and converts the best matching frame for an icon.
    fn load_image(&self, name: &str, scale: u32) -> Option<CursorImage> {
        let path = self.themes.iter().find_map(|theme| theme.load_icon(name))?;
        let data = std::fs::read(path).ok()?;
        let images = xcursor::parser::parse_xcursor(&data)?;

        // The nominal size closest to the requested one, its first frame.
        let wanted = self.size * scale;
        let size = images
            .iter()
            .map(|image| image.size)
            .min_by_key(|size| size.abs_diff(wanted))?;
        let image = images.iter().find(|image| image.size == size)?;
        // Themes without a big enough image get upscaled rather than drawn too small.
        let buffer_scale = (size / self.size.max(1)).clamp(1, scale.max(1)) as i32;

        let buffer = MemoryRenderBuffer::from_slice(
            &image.pixels_rgba,
            Fourcc::Abgr8888,
            (image.width as i32, image.height as i32),
            buffer_scale,
            Transform::Normal,
            None,
        );
        Some(CursorImage {
            buffer,
            hotspot: (
                image.xhot as i32 / buffer_scale,
                image.yhot as i32 / buffer_scale,
            )
                .into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> (CursorTheme, mpsc::Receiver<(CursorIcon, u32)>) {
        let (requests, pending) = mpsc::channel();
        let theme = CursorTheme {
            requests,
            cache: HashMap::new(),
        };
        (theme, pending)
    }

    fn image() -> CursorImage {
        CursorImage {
            buffer: MemoryRenderBuffer::default(),
            hotspot: (1, 2).into(),
        }
    }

    #[test]
    fn missing_icons_are_requested_once_and_the_arrow_stands_in() {
        let (mut theme, pending) = theme();
        assert!(theme.get(CursorIcon::Pointer, 2).is_none());
        assert!(theme.get(CursorIcon::Pointer, 2).is_none());
        let requested: Vec<_> = pending.try_iter().collect();
        assert_eq!(
            requested,
            [(CursorIcon::Pointer, 2), (CursorIcon::Default, 2)],
            "each requested once, without waiting"
        );

        theme
            .cache
            .insert((CursorIcon::Default, 2), Slot::Loaded(Some(image())));
        let arrow = theme
            .get(CursorIcon::Pointer, 2)
            .expect("the arrow meanwhile");
        assert_eq!(arrow.hotspot, (1, 2).into());

        // The theme has no such icon: the arrow, for good, and no new request.
        theme
            .cache
            .insert((CursorIcon::Pointer, 2), Slot::Loaded(None));
        assert!(theme.get(CursorIcon::Pointer, 2).is_some());
        assert_eq!(pending.try_iter().count(), 0);
    }

    #[test]
    fn preloading_asks_for_each_common_icon_at_that_scale() {
        let (mut theme, pending) = theme();
        theme.preload(1);
        theme.preload(1);
        let requested: Vec<_> = pending.try_iter().collect();
        assert_eq!(requested.len(), COMMON_ICONS.len());
        assert!(requested.iter().all(|&(_, scale)| scale == 1));
    }
}
