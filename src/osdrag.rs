//! Dragging files OUT of the app into other applications (Explorer, mail, browsers...).
//! Uses the `drag` crate (OLE drag source on Windows). This call is modal: it returns
//! when the drop finished or was cancelled.

use std::path::PathBuf;

#[cfg(windows)]
pub fn start(frame: &eframe::Frame, paths: Vec<PathBuf>) -> bool {
    let preview = include_bytes!("../assets/icon.ico").to_vec();
    drag::start_drag(
        frame,
        drag::DragItem::Files(paths),
        drag::Image::Raw(preview),
        |_result, _cursor| {},
        drag::Options::default(),
    )
    .is_ok()
}

#[cfg(not(windows))]
pub fn start(_frame: &eframe::Frame, _paths: Vec<PathBuf>) -> bool {
    false
}
