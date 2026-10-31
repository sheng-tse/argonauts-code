//! A view that draws a map: the screen puts pictures over the cells it leaves for them, where
//! the terminal draws pictures, and while the map shows the mouse points into it.

use std::sync::Arc;
use std::time::Instant;

use ratatui::layout::Rect;

use crate::base::draw::scene::{Image, Picture, Scene, picture_key};
use crate::frame::view::ViewCtx;

/// Where a view's map drew, in the buffer the view drew in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapCells {
    pub area: Rect,
    /// Whether the cells wait for pictures, and a number that changes whenever what they show
    /// does. `None` when the map is drawn in the cells themselves.
    pub picture: Option<u64>,
}

/// A small picture a map puts in cells it left blank beside a line it wrote, such as what the
/// pointer found, before the line under the map.
#[derive(Debug, Clone)]
pub struct Beside {
    /// The cells it covers, in the buffer the view drew in.
    pub area: Rect,
    /// Stretched over the cells; the screen keeps it by its key.
    pub picture: Picture,
}

/// Where the mouse rests over a map, from the top left of its cells: in pixels where the
/// terminal reports them, else at the middle of the cell, in the same units as the map's size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapPoint {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl MapPoint {
    pub fn inside(&self) -> bool {
        (0.0..self.width).contains(&self.x) && (0.0..self.height).contains(&self.y)
    }

    /// The point in a scene's pixels, where its hit map looks. A scene drawn at another cell
    /// size than the terminal's now, which the screen stretches over the cells until the next
    /// one is drawn, takes the point scaled to its size.
    pub fn in_scene(&self, scene: &Scene) -> (f32, f32) {
        let scale = |at: f32, of: f32, to: usize| {
            if of > 0.0 { at * to as f32 / of } else { at }
        };
        (
            scale(self.x, self.width, scene.width()),
            scale(self.y, self.height, scene.height()),
        )
    }
}

pub trait MapView {
    /// Whether the screen puts pictures over the cells left for them, at `cell` pixels a cell,
    /// so the map lays out for them at their shape rather than drawing in its cells.
    fn place_pictures(&mut self, cell: Option<(u16, u16)>);

    /// Where the map drew last, while it shows.
    fn map_cells(&self) -> Option<MapCells>;

    /// What the map shows at `cell` pixels a cell: layers and sprites stacked in depth, which the
    /// screen keeps by their keys and moves rather than sending again, and what the pointer finds
    /// where. The screen asks again when the key in [`MapCells`] changes, and on its clock while
    /// the last scene moves.
    ///
    /// A map drawn as one picture leaves this alone: its [`MapView::picture`] becomes the
    /// scene's only layer.
    fn scene(&self, cell: (u16, u16), ctx: &ViewCtx<'_>) -> Option<Arc<Scene>> {
        let key = self.map_cells()?.picture?;
        let image = Image::from_png(&self.picture(cell, ctx)?)?;
        let key = picture_key([key, u64::from(cell.0), u64::from(cell.1)]);
        Some(Arc::new(Scene::single(Picture::new(key, image))))
    }

    /// The map as one picture over its cells, a PNG at `cell` pixels a cell, for a map that
    /// draws a single picture rather than a scene.
    fn picture(&self, _cell: (u16, u16), _ctx: &ViewCtx<'_>) -> Option<Vec<u8>> {
        None
    }

    /// The mouse rests at `at` over the map, or has left it. Whether what shows changed.
    fn point(&mut self, at: Option<MapPoint>) -> bool;

    /// A click where the mouse rests. Whether it opened something.
    fn click(&mut self) -> bool;

    /// The wheel turned at `at` over the map, `up` away from you. Whether what shows changed.
    /// A map that doesn't zoom leaves it alone.
    fn wheel(&mut self, _at: MapPoint, _up: bool, _now: Instant) -> bool {
        false
    }

    /// The mouse moved to `to` with its button held down. Whether what shows changed. A map
    /// that nothing turns or moves leaves it alone.
    fn drag(&mut self, _to: MapPoint) -> bool {
        false
    }

    /// The picture beside a line the map wrote, while one shows, asked for every frame: a map
    /// keeps it rather than drawing it again.
    fn beside(&self) -> Option<Beside> {
        None
    }

    /// What the last drawing of the map's pictures to panic said, once, asked for every frame
    /// so the screen can say it. A map that draws in line never has one.
    fn take_failure(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::draw::scene::Depth;
    use crate::base::theme::Theme;
    use crate::frame::state::AppState;

    /// A map drawn as one picture, as the Behaviors tab draws today.
    struct OnePicture {
        key: Option<u64>,
        png: Option<Vec<u8>>,
    }

    impl MapView for OnePicture {
        fn place_pictures(&mut self, _cell: Option<(u16, u16)>) {}

        fn map_cells(&self) -> Option<MapCells> {
            Some(MapCells {
                area: Rect::new(0, 0, 2, 1),
                picture: self.key,
            })
        }

        fn picture(&self, _cell: (u16, u16), _ctx: &ViewCtx<'_>) -> Option<Vec<u8>> {
            self.png.clone()
        }

        fn point(&mut self, _at: Option<MapPoint>) -> bool {
            false
        }

        fn click(&mut self) -> bool {
            false
        }
    }

    fn scene_of(view: &OnePicture, cell: (u16, u16)) -> Option<Arc<Scene>> {
        let theme = Theme::dark();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        view.scene(cell, &ctx)
    }

    #[test]
    fn a_map_drawn_as_one_picture_hands_over_a_scene_of_it() {
        let image = Image::new(4, 2, [9, 8, 7, 255].repeat(8)).expect("eight pixels");
        let view = OnePicture {
            key: Some(3),
            png: Some(image.to_png()),
        };
        let scene = scene_of(&view, (2, 2)).expect("a scene");
        assert_eq!((scene.width(), scene.height()), (4, 2));
        let [layer] = scene.layers() else {
            panic!("one layer, not {:?}", scene.layers());
        };
        assert_eq!(layer.depth, Depth::Ground);
        assert_eq!(layer.picture.image, image);
        let larger = scene_of(&view, (4, 4)).expect("a scene");
        assert_ne!(larger.layers()[0].picture.key, layer.picture.key);
    }

    #[test]
    fn no_picture_and_no_key_make_no_scene() {
        let unkeyed = OnePicture {
            key: None,
            png: Some(Image::transparent(1, 1).to_png()),
        };
        assert!(scene_of(&unkeyed, (2, 2)).is_none());
        let blank = OnePicture {
            key: Some(1),
            png: None,
        };
        assert!(scene_of(&blank, (2, 2)).is_none());
    }

    #[test]
    fn a_point_is_scaled_into_a_scene_drawn_for_other_cells() {
        let point = MapPoint {
            x: 160.0,
            y: 42.0,
            width: 1280.0,
            height: 546.0,
        };
        assert_eq!(point.in_scene(&Scene::new(1280, 546)), (160.0, 42.0));
        assert_eq!(point.in_scene(&Scene::new(1152, 273)), (144.0, 21.0));
        let unmeasured = MapPoint {
            width: 0.0,
            ..point
        };
        assert_eq!(unmeasured.in_scene(&Scene::new(10, 546)).0, 160.0);
    }
}
