//! The cursor cropped to the window: a browser drops a custom cursor that crosses its edge.

use bevy::math::{URect, Vec2};

/// The part of a `size`×`size` cursor, its hotspot at the top-left, that fits in a window of
/// `window` logical px with the pointer at `pointer`; `None` when it fits whole or there is no
/// pointer. At least one pixel each way is kept.
pub(crate) fn edge_crop(pointer: Option<Vec2>, window: Vec2, size: u32) -> Option<URect> {
    let pointer = pointer?;
    let fit = |room: f32| (room.floor().max(1.0) as u32).min(size);
    let (w, h) = (fit(window.x - pointer.x), fit(window.y - pointer.y));
    (w < size || h < size).then(|| URect::new(0, 0, w, h))
}

#[cfg(test)]
mod tests {
    use super::edge_crop;
    use bevy::math::{URect, Vec2};

    const WINDOW: Vec2 = Vec2::new(1280.0, 720.0);

    fn crop(x: f32, y: f32) -> Option<URect> {
        edge_crop(Some(Vec2::new(x, y)), WINDOW, 32)
    }

    #[test]
    fn a_cursor_with_room_is_whole() {
        assert_eq!(crop(100.0, 100.0), None);
        assert_eq!(crop(1248.0, 688.0), None, "exactly 32 px of room each way");
        assert_eq!(edge_crop(None, WINDOW, 32), None, "no pointer");
    }

    #[test]
    fn near_the_bottom_edge_only_the_rows_that_fit_show() {
        assert_eq!(crop(100.0, 700.0), Some(URect::new(0, 0, 32, 20)));
        assert_eq!(crop(100.0, 689.0), Some(URect::new(0, 0, 32, 31)));
        // A fractional pointer (a HiDPI screen) rounds the room down, so the image stays inside.
        assert_eq!(crop(100.0, 700.5), Some(URect::new(0, 0, 32, 19)));
    }

    #[test]
    fn near_the_right_edge_and_the_corner_it_crops_each_way() {
        assert_eq!(crop(1270.0, 100.0), Some(URect::new(0, 0, 10, 32)));
        assert_eq!(crop(1270.0, 710.0), Some(URect::new(0, 0, 10, 10)));
    }

    #[test]
    fn at_the_last_pixel_or_past_the_edge_one_pixel_is_kept() {
        assert_eq!(crop(1279.5, 719.5), Some(URect::new(0, 0, 1, 1)));
        assert_eq!(crop(1300.0, 800.0), Some(URect::new(0, 0, 1, 1)));
    }
}
