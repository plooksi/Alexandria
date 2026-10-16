//! Window geometry persisted in %APPDATA%\Alexandria\window.json so it can be applied
//! before the window is created (independent of eframe's storage key).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct WinState {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub maximized: bool,
}

fn file() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("Alexandria").join("window.json"))
}

impl WinState {
    pub fn load() -> Option<Self> {
        let w: WinState = serde_json::from_str(&std::fs::read_to_string(file()?).ok()?).ok()?;
        let sane = w.size[0] >= 400.0
            && w.size[1] >= 300.0
            && w.pos[0] > -w.size[0] + 100.0 // not stranded off-screen to the left
            && w.pos[1] > -20.0
            && w.pos[0] < 30000.0
            && w.pos[1] < 30000.0;
        sane.then_some(w)
    }

    pub fn save(&self) {
        if let Some(p) = file() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            if let Ok(s) = serde_json::to_string(self) {
                let _ = std::fs::write(p, s);
            }
        }
    }
}
