//! Tray icon images, drawn in code (spec §17.2).
//!
//! Responsibility: produce a small RGBA image per [`GateIndicator`]: a coloured
//! disc with a light gate bar, anti-aliased by supersampling.
//! Not responsible for: choosing the state (see `tray_model`).

use gatekeeper_core::gate::model_state::GateIndicator;
use tauri::image::Image;

/// Side length of the generated icons. Windows scales tray icons to the
/// display DPI, so 32 px looks sharp at 100%-200% scaling.
const SIZE: u32 = 32;

/// Supersampling per axis.
const SS: u32 = 4;

/// Fill colour for each state.
const fn colour(state: GateIndicator) -> [u8; 3] {
    match state {
        GateIndicator::Open => [34, 197, 94],     // green
        GateIndicator::Busy => [59, 130, 246],    // blue
        GateIndicator::Draining => [234, 179, 8], // yellow
        GateIndicator::Closed => [239, 68, 68],   // red
        GateIndicator::Held => [168, 85, 247],    // purple
        GateIndicator::Paused => [148, 163, 184], // grey
    }
}

/// Draws the icon for `state`.
pub fn icon_for(state: GateIndicator) -> Image<'static> {
    let rgb = colour(state);
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let centre = f64::from(SIZE) / 2.0;
    let radius = centre - 1.0;

    for y in 0..SIZE {
        for x in 0..SIZE {
            let (mut disc, mut bar) = (0u32, 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = f64::from(x) + (f64::from(sx) + 0.5) / f64::from(SS);
                    let py = f64::from(y) + (f64::from(sy) + 0.5) / f64::from(SS);
                    if (px - centre).hypot(py - centre) <= radius {
                        disc += 1;
                        // The gate bar across the middle of the disc.
                        if (py - centre).abs() <= 2.2 && (px - centre).abs() <= radius * 0.62 {
                            bar += 1;
                        }
                    }
                }
            }
            let total = SS * SS;
            let i = ((y * SIZE + x) * 4) as usize;
            let mix = |base: u8| -> u8 {
                // Blend the disc colour with white where the bar covers.
                let covered = f64::from(bar) / f64::from(disc.max(1));
                (f64::from(base) * (1.0 - covered) + 250.0 * covered) as u8
            };
            rgba[i] = mix(rgb[0]);
            rgba[i + 1] = mix(rgb[1]);
            rgba[i + 2] = mix(rgb[2]);
            rgba[i + 3] = (255 * disc / total) as u8;
        }
    }
    Image::new_owned(rgba, SIZE, SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_has_a_distinct_opaque_centre() {
        let states = [
            GateIndicator::Open,
            GateIndicator::Busy,
            GateIndicator::Draining,
            GateIndicator::Closed,
            GateIndicator::Held,
            GateIndicator::Paused,
        ];
        // A pixel just above the bar shows the pure state colour.
        let probe = |s: GateIndicator| {
            let img = icon_for(s);
            let i = (((SIZE / 2 - 6) * SIZE + SIZE / 2) * 4) as usize;
            let px = &img.rgba()[i..i + 4];
            assert_eq!(px[3], 255, "{s:?} is opaque");
            [px[0], px[1], px[2]]
        };
        let colours: Vec<[u8; 3]> = states.iter().map(|s| probe(*s)).collect();
        for (i, a) in colours.iter().enumerate() {
            for b in &colours[i + 1..] {
                assert_ne!(a, b, "states must be visually distinct");
            }
        }
    }

    #[test]
    fn corners_are_transparent() {
        let img = icon_for(GateIndicator::Open);
        assert_eq!(img.rgba()[3], 0);
    }
}
