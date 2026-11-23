use kairos_core::brand::MARK;

#[cfg(any(test, not(target_os = "macos")))]
const FIELD: [u8; 3] = [0x3a, 0x33, 0xa3];
#[cfg(any(test, not(target_os = "macos")))]
const PAUSED_FIELD: [u8; 3] = [0x8a, 0x87, 0x99];
#[cfg(any(test, not(target_os = "macos")))]
const WHITE: [u8; 3] = [0xff, 0xff, 0xff];
const RING_ALPHA: u32 = 107;
/// Template images are tinted by the system, so a paused mark is just fainter.
#[cfg(any(test, target_os = "macos"))]
const PAUSED_FADE: u32 = 45;

pub struct Image {
    pub size: u32,
    pub rgba: Vec<u8>,
}

fn coverage(x: u32, y: u32, size: u32) -> u32 {
    let margin = size / 16;
    let span = size.saturating_sub(2 * margin);
    let inner = |v: u32| v.checked_sub(margin).filter(|v| *v < span);
    let (Some(x), Some(y)) = (inner(x), inner(y)) else { return 0 };
    let cell = MARK.get((y * 9 / span) as usize).and_then(|row| row.as_bytes().get((x * 9 / span) as usize));
    match cell {
        Some(b'#') => 255,
        Some(b'o') => RING_ALPHA,
        _ => 0,
    }
}

#[cfg(any(test, not(target_os = "macos")))]
fn mix(from: u8, to: u8, alpha: u32) -> u8 {
    ((from as u32 * (255 - alpha) + to as u32 * alpha + 127) / 255) as u8
}

#[cfg(any(test, not(target_os = "macos")))]
pub fn on_field(paused: bool, size: u32) -> Image {
    let field = if paused { PAUSED_FIELD } else { FIELD };
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let a = coverage(x, y, size);
            rgba.extend(field.iter().zip(WHITE).map(|(f, w)| mix(*f, w, a)));
            rgba.push(255);
        }
    }
    Image { size, rgba }
}

#[cfg(any(test, target_os = "macos"))]
pub fn template(paused: bool, size: u32) -> Image {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let mut a = coverage(x, y, size);
            if paused {
                a = a * PAUSED_FADE / 100;
            }
            rgba.extend([0, 0, 0, a as u8]);
        }
    }
    Image { size, rgba }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(img: &Image, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * img.size + x) * 4) as usize;
        [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2], img.rgba[i + 3]]
    }

    #[test]
    fn buffers_match_their_size() {
        for size in [16, 32, 64] {
            assert_eq!(on_field(false, size).rgba.len(), (size * size * 4) as usize);
            assert_eq!(template(true, size).rgba.len(), (size * size * 4) as usize);
        }
    }

    #[test]
    fn field_icon_is_white_on_brand_blue() {
        let img = on_field(false, 32);
        assert_eq!(px(&img, 0, 0), [0x3a, 0x33, 0xa3, 255]);
        assert_eq!(px(&img, 31, 31), [0x3a, 0x33, 0xa3, 255]);
        // Margin 2, span 28. Row 0 of the mark is "...o##...": ring at column 3, solid at 4.
        let solid = px(&img, 2 + 4 * 28 / 9 + 1, 2 + 1);
        assert_eq!(solid, [255, 255, 255, 255]);
        let ring = px(&img, 2 + 3 * 28 / 9 + 1, 2 + 1);
        assert!(ring[0] > 0x3a && ring[0] < 255, "ring should be a blend, got {ring:?}");
        assert_eq!(px(&img, 16, 22), [0x3a, 0x33, 0xa3, 255]);
    }

    #[test]
    fn paused_field_is_grey() {
        assert_eq!(px(&on_field(true, 32), 0, 0), [0x8a, 0x87, 0x99, 255]);
    }

    #[test]
    fn template_is_black_with_alpha_only() {
        let img = template(false, 64);
        assert!(img.rgba.chunks(4).all(|p| p[..3] == [0, 0, 0]));
        assert_eq!(px(&img, 0, 0)[3], 0);
        assert!(img.rgba.chunks(4).any(|p| p[3] == 255));
        assert!(img.rgba.chunks(4).any(|p| p[3] == 107));
    }

    #[test]
    fn paused_template_is_fainter() {
        let loud = template(false, 64);
        let quiet = template(true, 64);
        let max = |i: &Image| i.rgba.chunks(4).map(|p| p[3]).max().unwrap();
        assert_eq!(max(&loud), 255);
        assert!(max(&quiet) < 255 && max(&quiet) > 0);
    }

    #[test]
    fn tiny_sizes_do_not_panic() {
        for size in [0, 1, 2, 9, 15] {
            assert_eq!(on_field(false, size).rgba.len(), (size * size * 4) as usize);
        }
    }
}
