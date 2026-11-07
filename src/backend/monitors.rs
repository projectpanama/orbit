use std::{collections::HashSet, fmt};

use orbit_config::ModeSpec;
use smithay::{
    output::{Mode as OutputMode, Subpixel},
    reexports::drm::control::{
        Device as ControlDevice, Mode, ModeFlags, ModeTypeFlags, connector, crtc,
    },
};

/// Physical sizes below this value are treated as invalid.
///
/// Projectors and some televisions can report missing or unusable sizes.
const MIN_PLAUSIBLE_MM: i32 = 100;
/// Pixel density (per inch) that should look like scale 1. Laptop panels
/// are viewed closer than desktop monitors, so they get a higher one.
const REFERENCE_PPI_EXTERNAL: f64 = 110.0;
const REFERENCE_PPI_INTERNAL: f64 = 125.0;
/// How many modes `fallback_modes` offers, the first included.
const MODE_ATTEMPTS: usize = 6;

/// A connected display.
#[derive(Debug, Clone)]
pub struct Monitor {
    /// DRM connector handle.
    pub connector: connector::Handle,
    /// Kernel-style name, e.g. `eDP-1` or `HDMI-A-1`.
    pub name: String,
    /// Connector interface type.
    pub interface: connector::Interface,
    /// Display manufacturer.
    pub make: String,
    /// Display model name.
    pub model: String,
    /// Display serial number when available.
    pub serial: Option<String>,
    /// Physical size in millimetres, if the display reports one.
    pub size_mm: (i32, i32),
    /// Subpixel arrangement reported by the connector.
    pub subpixel: Subpixel,
    /// Modes reported by the connector.
    pub modes: Vec<Mode>,
    /// The CRTCs able to drive this connector, free or not.
    pub possible_crtcs: Vec<crtc::Handle>,
}

impl Monitor {
    /// Reports whether the monitor uses an internal panel interface.
    pub fn is_internal(&self) -> bool {
        matches!(
            self.interface,
            connector::Interface::EmbeddedDisplayPort
                | connector::Interface::LVDS
                | connector::Interface::DSI
        )
    }

    /// Returns a stable description based on make model and serial.
    pub fn description(&self) -> String {
        format!(
            "{} {} {}",
            self.make,
            self.model,
            self.serial.as_deref().unwrap_or("Unknown")
        )
    }
}

impl fmt::Display for Monitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} {} (", self.name, self.make, self.model)?;
        if let Some(mode) = pick_mode(&self.modes, None) {
            let mode = OutputMode::from(mode);
            write!(
                f,
                "{}x{} @ {:.3} Hz, ",
                mode.size.w,
                mode.size.h,
                mode.refresh as f64 / 1000.0
            )?;
        }
        write!(
            f,
            "{}x{} mm{})",
            self.size_mm.0,
            self.size_mm.1,
            if self.is_internal() { ", internal" } else { "" },
        )
    }
}

/// EDID fields needed to identify a monitor.
#[derive(Debug, PartialEq, Eq)]
struct Edid {
    /// Manufacturer identifier.
    make: String,
    /// Model name or product code.
    model: String,
    /// Monitor serial number when present.
    serial: Option<String>,
}

impl Edid {
    const HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];

    /// Parses monitor identity fields from an EDID base block.
    fn parse(edid: &[u8]) -> Option<Self> {
        if edid.len() < 128 || edid[..8] != Self::HEADER {
            return None;
        }

        // Three 5-bit letters, 'A' = 1, packed big-endian into bytes 8-9.
        let packed = u16::from_be_bytes([edid[8], edid[9]]);
        let make: String = [10, 5, 0]
            .iter()
            .map(|shift| (b'A' - 1 + ((packed >> shift) & 0x1f) as u8) as char)
            .collect();
        let product = u16::from_le_bytes([edid[10], edid[11]]);

        // Display descriptors live in four 18-byte blocks from byte 54. A
        // zero pixel clock marks a descriptor rather than a timing.
        let mut name = None;
        let mut text_string = None;
        let mut serial = None;
        for block in edid[54..126].as_chunks::<18>().0 {
            if block[0..2] != [0, 0] {
                continue;
            }
            let text = || {
                let raw = &block[5..18];
                let end = raw.iter().position(|&b| b == b'\n').unwrap_or(raw.len());
                // EDID text is ASCII and some panels pad it with junk.
                let text = raw[..end]
                    .iter()
                    .map(|&b| if b.is_ascii_graphic() { b as char } else { ' ' })
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                (!text.is_empty()).then_some(text)
            };
            match block[3] {
                0xfc => name = text(),
                // Laptop panels often put their model here instead.
                0xfe => text_string = text_string.or_else(text),
                0xff => serial = text(),
                _ => {}
            }
        }

        Some(Self {
            make,
            model: name
                .or(text_string)
                .unwrap_or_else(|| format!("0x{product:04X}")),
            serial,
        })
    }
}

/// Discovers every connected connector in kernel order.
///
/// A forced probe asks the kernel to redetect each connector.
///
/// This can flicker active displays and is needed only while Orbit owns the device.
pub fn discover(drm: &impl ControlDevice, force_probe: bool) -> std::io::Result<Vec<Monitor>> {
    let resources = drm.resource_handles()?;
    let mut monitors = Vec::new();

    for &handle in resources.connectors() {
        let info = match drm.get_connector(handle, force_probe) {
            Ok(info) => info,
            Err(err) => {
                tracing::warn!(?handle, %err, "failed to query connector");
                continue;
            }
        };
        if info.state() != connector::State::Connected || info.modes().is_empty() {
            continue;
        }

        let name = connector_name(info.interface(), info.interface_id());
        let edid = read_edid(drm, handle).and_then(|blob| Edid::parse(&blob));
        let (make, model, serial) = match edid {
            Some(edid) => (edid.make, edid.model, edid.serial),
            None => ("Unknown".to_string(), name.clone(), None),
        };

        let mut possible_crtcs: Vec<crtc::Handle> = info
            .encoders()
            .iter()
            .filter_map(|&encoder| drm.get_encoder(encoder).ok())
            .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
            .collect();
        // Encoders often share CRTCs.
        let mut seen = HashSet::new();
        possible_crtcs.retain(|crtc| seen.insert(*crtc));

        let (w, h) = info.size().unwrap_or((0, 0));
        monitors.push(Monitor {
            connector: handle,
            name,
            interface: info.interface(),
            make,
            model,
            serial,
            size_mm: (w as i32, h as i32),
            subpixel: info.subpixel().into(),
            modes: info.modes().to_vec(),
            possible_crtcs,
        });
    }

    Ok(monitors)
}

/// Returns the first available CRTC that can drive a monitor.
pub fn assign_crtc(
    possible: &[crtc::Handle],
    in_use: &HashSet<crtc::Handle>,
) -> Option<crtc::Handle> {
    possible.iter().copied().find(|crtc| !in_use.contains(crtc))
}

/// Builds the standard connector name from its interface and identifier.
pub fn connector_name(interface: connector::Interface, id: u32) -> String {
    format!("{}-{}", interface.as_str(), id)
}

/// Returns fallback modes to try after the preferred mode fails.
///
/// The preferred mode comes first, followed by lower refresh rates and lower
/// resolutions. Interlaced and duplicate modes are skipped.
pub fn fallback_modes(modes: &[Mode], first: Mode) -> Vec<Mode> {
    let refresh = |mode: &Mode| OutputMode::from(*mode).refresh;
    let area = |mode: &Mode| {
        let (w, h) = mode.size();
        u32::from(w) * u32::from(h)
    };
    let mut rest: Vec<Mode> = modes
        .iter()
        .copied()
        .filter(|m| !m.flags().contains(ModeFlags::INTERLACE))
        .filter(|m| {
            if m.size() == first.size() {
                refresh(m) < refresh(&first)
            } else {
                area(m) < area(&first)
            }
        })
        .collect();
    rest.sort_by_key(|m| {
        (
            m.size() != first.size(),
            std::cmp::Reverse(area(m)),
            std::cmp::Reverse(refresh(m)),
        )
    });
    rest.dedup_by_key(|m| (m.size(), refresh(m)));
    std::iter::once(first)
        .chain(rest)
        .take(MODE_ATTEMPTS)
        .collect()
}

/// Selects the best mode for a display.
///
/// Without `spec`, uses the preferred resolution or the largest available
/// resolution at its highest refresh rate.
///
/// With `spec`, uses the requested resolution at the closest refresh rate.
/// An unsupported specification falls back to the default. Returns `None`
/// only when `modes` is empty.
pub fn pick_mode(modes: &[Mode], spec: Option<&ModeSpec>) -> Option<Mode> {
    let refresh = |mode: &Mode| OutputMode::from(*mode).refresh;
    // Prefer progressive modes when a resolution offers them.
    let best_at = |size: (u16, u16), wanted: Option<i32>| {
        let at_size = || modes.iter().filter(move |m| m.size() == size);
        let progressive: Vec<&Mode> = at_size()
            .filter(|m| !m.flags().contains(ModeFlags::INTERLACE))
            .collect();
        let candidates = if progressive.is_empty() {
            at_size().collect()
        } else {
            progressive
        };
        match wanted {
            Some(wanted) => candidates
                .into_iter()
                .min_by_key(|m| (refresh(m) - wanted).abs()),
            None => candidates.into_iter().max_by_key(|m| refresh(m)),
        }
        .copied()
    };

    if let Some(spec) = spec {
        if let Some(mode) = best_at((spec.width, spec.height), spec.refresh_mhz) {
            return Some(mode);
        }
        tracing::warn!(
            "mode {}x{} is unsupported, using the default",
            spec.width,
            spec.height
        );
    }

    let preferred = modes
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| {
            modes.iter().max_by_key(|m| {
                let (w, h) = m.size();
                (u32::from(w) * u32::from(h), refresh(m))
            })
        })?;
    best_at(preferred.size(), None).or(Some(*preferred))
}

/// Calculates an automatic display scale from pixel density.
///
/// The result uses quarter steps between one and three. Invalid physical
/// dimensions fall back to one.
pub fn auto_scale(size_px: (i32, i32), size_mm: (i32, i32), internal: bool) -> f64 {
    let (w_px, h_px) = size_px;
    let (w_mm, h_mm) = size_mm;
    if w_px <= 0 || h_px <= 0 || w_mm < MIN_PLAUSIBLE_MM || h_mm < MIN_PLAUSIBLE_MM {
        return 1.0;
    }
    // Reject physical sizes with an implausible aspect ratio.
    let px_ratio = f64::from(w_px) / f64::from(h_px);
    let mm_ratio = f64::from(w_mm) / f64::from(h_mm);
    if (px_ratio / mm_ratio - 1.0).abs() > 0.25 {
        return 1.0;
    }

    let diagonal_px = f64::from(w_px).hypot(f64::from(h_px));
    let diagonal_in = f64::from(w_mm).hypot(f64::from(h_mm)) / 25.4;
    let ppi = diagonal_px / diagonal_in;
    let reference = if internal {
        REFERENCE_PPI_INTERNAL
    } else {
        REFERENCE_PPI_EXTERNAL
    };
    ((ppi / reference * 4.0).round() / 4.0).clamp(1.0, 3.0)
}

fn read_edid(drm: &impl ControlDevice, connector: connector::Handle) -> Option<Vec<u8>> {
    let props = drm.get_properties(connector).ok()?;
    let (handles, values) = props.as_props_and_values();
    handles.iter().zip(values).find_map(|(&handle, &value)| {
        let info = drm.get_property(handle).ok()?;
        if info.name().to_str().ok()? != "EDID" || value == 0 {
            return None;
        }
        drm.get_property_blob(value).ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an EDID base block for manufacturer "DEL", product 0xA06E,
    /// with the given 18-byte display descriptors.
    fn edid(descriptors: &[(u8, &str)]) -> [u8; 128] {
        let mut edid = [0u8; 128];
        edid[..8].copy_from_slice(&Edid::HEADER);
        edid[8..10].copy_from_slice(&[0x10, 0xac]);
        edid[10..12].copy_from_slice(&0xa06e_u16.to_le_bytes());
        for (index, (tag, text)) in descriptors.iter().enumerate() {
            let block = &mut edid[54 + index * 18..72 + index * 18];
            block[3] = *tag;
            let mut payload = [b' '; 13];
            payload[..text.len()].copy_from_slice(text.as_bytes());
            if text.len() < 13 {
                payload[text.len()] = b'\n';
            }
            block[5..18].copy_from_slice(&payload);
        }
        edid
    }

    #[test]
    fn parses_make_model_and_serial() {
        let blob = edid(&[(0xff, "CFV9N57C05HL"), (0xfc, "DELL U2415")]);
        assert_eq!(
            Edid::parse(&blob),
            Some(Edid {
                make: "DEL".into(),
                model: "DELL U2415".into(),
                serial: Some("CFV9N57C05HL".into()),
            })
        );
    }

    #[test]
    fn falls_back_to_product_code_without_name() {
        let parsed = Edid::parse(&edid(&[(0xff, "123")])).unwrap();
        assert_eq!(parsed.model, "0xA06E");
        assert_eq!(parsed.serial.as_deref(), Some("123"));
    }

    #[test]
    fn uses_text_string_when_unnamed() {
        let parsed = Edid::parse(&edid(&[(0xfe, "BOE CQ"), (0xfe, "NE160QDM-NY1")])).unwrap();
        assert_eq!(parsed.model, "BOE CQ");
        let named = Edid::parse(&edid(&[(0xfe, "BOE CQ"), (0xfc, "Real Name")])).unwrap();
        assert_eq!(named.model, "Real Name");
    }

    #[test]
    fn drops_non_ascii_bytes() {
        let mut blob = edid(&[(0xfe, "NCJPF NE16N41")]);
        blob[54 + 5 + 5] = 0x80;
        assert_eq!(Edid::parse(&blob).unwrap().model, "NCJPF NE16N41");
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(Edid::parse(&[0u8; 128]), None);
        assert_eq!(Edid::parse(&edid(&[])[..64]), None);
    }

    fn mode(w: u16, h: u16, hz: u32, preferred: bool) -> Mode {
        use drm_ffi::drm_mode_modeinfo;
        // Pixel clock (kHz) for exactly `hz` with no blanking.
        let clock = u32::from(w) * u32::from(h) * hz / 1000;
        let mut info = drm_mode_modeinfo {
            clock,
            hdisplay: w,
            htotal: w,
            vdisplay: h,
            vtotal: h,
            vrefresh: hz,
            ..Default::default()
        };
        if preferred {
            info.type_ = ModeTypeFlags::PREFERRED.bits();
        }
        Mode::from(info)
    }

    fn summary(mode: Option<Mode>) -> Option<(u16, u16, i32)> {
        mode.map(|m| {
            let (w, h) = m.size();
            (w, h, (OutputMode::from(m).refresh + 500) / 1000)
        })
    }

    #[test]
    fn default_mode_is_preferred_size_at_highest_refresh() {
        let modes = [
            mode(3840, 2160, 60, false),
            mode(2560, 1440, 60, true),
            mode(2560, 1440, 144, false),
            mode(2560, 1440, 120, false),
            mode(1920, 1080, 240, false),
        ];
        assert_eq!(summary(pick_mode(&modes, None)), Some((2560, 1440, 144)));
    }

    #[test]
    fn fallback_lowers_refresh_then_resolution() {
        let modes = [
            mode(3840, 2160, 60, false),
            mode(2560, 1440, 60, true),
            mode(2560, 1440, 144, false),
            mode(2560, 1440, 120, false),
            mode(2560, 1440, 120, false),
            mode(1920, 1080, 240, false),
            mode(1920, 1080, 60, false),
            mode(1280, 720, 60, false),
            mode(1024, 768, 60, false),
        ];
        let first = pick_mode(&modes, None).unwrap();
        let order: Vec<_> = fallback_modes(&modes, first)
            .into_iter()
            .map(|m| summary(Some(m)).unwrap())
            .collect();
        assert_eq!(
            order,
            [
                (2560, 1440, 144),
                (2560, 1440, 120),
                (2560, 1440, 60),
                (1920, 1080, 240),
                (1920, 1080, 60),
                (1280, 720, 60),
            ]
        );
        // The lowest mode has nothing to fall back to.
        let lowest = mode(1024, 768, 60, false);
        assert_eq!(fallback_modes(&modes, lowest).len(), 1);
    }

    #[test]
    fn default_mode_without_preferred_is_largest() {
        let modes = [mode(1920, 1080, 60, false), mode(3840, 2160, 30, false)];
        assert_eq!(summary(pick_mode(&modes, None)), Some((3840, 2160, 30)));
        assert_eq!(pick_mode(&[], None), None);
    }

    #[test]
    fn configured_mode_picks_closest_refresh() {
        let modes = [
            mode(2560, 1440, 60, true),
            mode(2560, 1440, 144, false),
            mode(1920, 1080, 60, false),
            mode(1920, 1080, 120, false),
        ];
        let spec = |w, h, hz: Option<i32>| ModeSpec {
            width: w,
            height: h,
            refresh_mhz: hz.map(|hz| hz * 1000),
        };
        assert_eq!(
            summary(pick_mode(&modes, Some(&spec(1920, 1080, Some(100))))),
            Some((1920, 1080, 120))
        );
        assert_eq!(
            summary(pick_mode(&modes, Some(&spec(1920, 1080, None)))),
            Some((1920, 1080, 120))
        );
        assert_eq!(
            summary(pick_mode(&modes, Some(&spec(2560, 1440, Some(59))))),
            Some((2560, 1440, 60))
        );
        // Use the default when the requested mode is unsupported.
        assert_eq!(
            summary(pick_mode(&modes, Some(&spec(1280, 720, None)))),
            Some((2560, 1440, 144))
        );
    }

    #[test]
    fn assigns_free_crtcs() {
        use smithay::reexports::drm::control::RawResourceHandle;
        let a = crtc::Handle::from(RawResourceHandle::new(1).unwrap());
        let b = crtc::Handle::from(RawResourceHandle::new(2).unwrap());
        assert_eq!(assign_crtc(&[a, b], &HashSet::new()), Some(a));
        assert_eq!(assign_crtc(&[a, b], &HashSet::from([a])), Some(b));
        assert_eq!(assign_crtc(&[a], &HashSet::from([a])), None);
    }

    #[test]
    fn auto_scale_from_density() {
        // 24" 1080p and 27" 1440p desktop monitors.
        assert_eq!(auto_scale((1920, 1080), (531, 299), false), 1.0);
        assert_eq!(auto_scale((2560, 1440), (597, 336), false), 1.0);
        // 27" and 32" 4K.
        assert_eq!(auto_scale((3840, 2160), (597, 336), false), 1.5);
        assert_eq!(auto_scale((3840, 2160), (708, 398), false), 1.25);
        // 14" 2880x1800 and 13.3" 1080p laptop panels.
        assert_eq!(auto_scale((2880, 1800), (302, 189), true), 2.0);
        assert_eq!(auto_scale((1920, 1080), (294, 165), true), 1.25);
        // Unknown or made-up sizes.
        assert_eq!(auto_scale((3840, 2160), (0, 0), false), 1.0);
        assert_eq!(auto_scale((3840, 2160), (160, 90), false), 1.0);
        assert_eq!(auto_scale((3840, 2160), (1600, 300), false), 1.0);
    }

    #[test]
    fn names_connectors_like_the_kernel() {
        assert_eq!(
            connector_name(connector::Interface::EmbeddedDisplayPort, 1),
            "eDP-1"
        );
        assert_eq!(connector_name(connector::Interface::HDMIA, 2), "HDMI-A-2");
    }
}
