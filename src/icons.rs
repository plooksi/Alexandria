//! System (shell) icons for files and folders, fetched on a worker thread through
//! SHGetFileInfo and cached as egui textures. Until an icon arrives (or if it fails)
//! the caller falls back to the painted icons from `theme`.
//!
//! Cache strategy: folders share one icon; ordinary files are cached per extension
//! (no disk access); exe/lnk/ico/etc., drives and quick-access folders are cached per path.

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use eframe::egui::{self, pos2, Color32, ColorImage, Painter, Rect, TextureHandle, TextureOptions};

use crate::fsx::Entry;

#[derive(Clone)]
enum Key {
    Dir,
    Ext(String),
    Path(PathBuf),
    Recycle,
}

pub enum Spec<'a> {
    Dir,
    Ext(&'a str),
    Path(&'a Path),
    Recycle,
}

impl<'a> Spec<'a> {
    pub fn for_entry(e: &'a Entry) -> Spec<'a> {
        if e.is_dir {
            if e.path.parent().is_none() {
                Spec::Path(&e.path) // drive root
            } else {
                Spec::Dir
            }
        } else if matches!(
            e.ext.as_str(),
            "exe" | "ico" | "lnk" | "scr" | "url" | "msi" | "cur" | "ani" | "appx" | "msix" | "cpl"
        ) {
            Spec::Path(&e.path) // these carry per-file icons
        } else {
            Spec::Ext(&e.ext)
        }
    }
}

struct Req {
    key: Key,
    large: bool,
}
struct Res {
    key: Key,
    img: Option<(usize, usize, Vec<u8>)>,
}

enum Slot {
    Pending,
    Ready(TextureHandle),
    Failed,
}

struct Inner {
    dir: Option<Slot>,
    recycle: Option<Slot>,
    ext: HashMap<String, Slot>,
    path: HashMap<PathBuf, Slot>,
    large: bool,
    counter: u64,
    rx: Receiver<Res>,
}

pub struct Icons {
    inner: RefCell<Inner>,
    tx: Sender<Req>,
}

impl Icons {
    pub fn new(ctx: &egui::Context) -> Self {
        let (tx, req_rx) = mpsc::channel::<Req>();
        let (res_tx, rx) = mpsc::channel::<Res>();
        let c = ctx.clone();
        thread::spawn(move || {
            shell::init();
            while let Ok(r) = req_rx.recv() {
                let img = shell::load(&r.key, r.large);
                if res_tx.send(Res { key: r.key, img }).is_err() {
                    break;
                }
                c.request_repaint();
            }
        });
        Icons {
            inner: RefCell::new(Inner {
                dir: None,
                recycle: None,
                ext: HashMap::new(),
                path: HashMap::new(),
                large: ctx.pixels_per_point() > 1.25,
                counter: 0,
                rx,
            }),
            tx,
        }
    }

    /// Call once per frame: uploads finished icons and re-fetches at the right size on DPI changes.
    pub fn poll(&self, ctx: &egui::Context) {
        let mut inn = self.inner.borrow_mut();
        let large = ctx.pixels_per_point() > 1.25;
        if large != inn.large {
            inn.large = large;
            inn.dir = None;
            inn.recycle = None;
            inn.ext.clear();
            inn.path.clear();
        }
        while let Ok(res) = inn.rx.try_recv() {
            let slot = match res.img {
                Some((w, h, px)) => {
                    inn.counter += 1;
                    let name = format!("sysicon{}", inn.counter);
                    let image = ColorImage::from_rgba_unmultiplied([w, h], &px);
                    Slot::Ready(ctx.load_texture(name, image, TextureOptions::LINEAR))
                }
                None => Slot::Failed,
            };
            match res.key {
                Key::Dir => inn.dir = Some(slot),
                Key::Recycle => inn.recycle = Some(slot),
                Key::Ext(e) => {
                    inn.ext.insert(e, slot);
                }
                Key::Path(p) => {
                    inn.path.insert(p, slot);
                }
            }
        }
    }

    /// Paints the system icon into `rect`. Returns false when it is not available (yet),
    /// in which case the caller should paint its fallback icon.
    pub fn paint(&self, p: &Painter, rect: Rect, spec: Spec) -> bool {
        let mut inn = self.inner.borrow_mut();
        let slot = match &spec {
            Spec::Dir => inn.dir.as_ref(),
            Spec::Recycle => inn.recycle.as_ref(),
            Spec::Ext(e) => inn.ext.get(*e),
            Spec::Path(pa) => inn.path.get(*pa),
        };
        match slot {
            Some(Slot::Ready(t)) => {
                p.image(t.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                return true;
            }
            Some(_) => return false,
            None => {}
        }
        let key = match spec {
            Spec::Dir => {
                inn.dir = Some(Slot::Pending);
                Key::Dir
            }
            Spec::Recycle => {
                inn.recycle = Some(Slot::Pending);
                Key::Recycle
            }
            Spec::Ext(e) => {
                inn.ext.insert(e.to_string(), Slot::Pending);
                Key::Ext(e.to_string())
            }
            Spec::Path(pa) => {
                inn.path.insert(pa.to_path_buf(), Slot::Pending);
                Key::Path(pa.to_path_buf())
            }
        };
        let _ = self.tx.send(Req { key, large: inn.large });
        false
    }
}

#[cfg(windows)]
mod shell {
    use super::Key;
    use std::{
        ffi::{c_void, OsStr},
        mem::{size_of, zeroed},
        os::windows::ffi::OsStrExt,
    };
    use windows_sys::Win32::{
        Graphics::Gdi::{
            DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, HBITMAP, HDC,
        },
        System::Com::CoInitializeEx,
        UI::{
            Shell::{SHGetFileInfoW, SHGetStockIconInfo, SHFILEINFOW, SHSTOCKICONINFO},
            WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO},
        },
    };

    const SHGFI_ICON: u32 = 0x100;
    const SHGFI_SMALLICON: u32 = 0x1;
    const SHGFI_USEFILEATTRIBUTES: u32 = 0x10;
    const ATTR_NORMAL: u32 = 0x80;
    const ATTR_DIRECTORY: u32 = 0x10;

    pub fn init() {
        unsafe {
            CoInitializeEx(std::ptr::null(), 2); // COINIT_APARTMENTTHREADED
        }
    }

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }

    pub fn load(key: &Key, large: bool) -> Option<(usize, usize, Vec<u8>)> {
        if let Key::Recycle = key {
            unsafe {
                let mut si: SHSTOCKICONINFO = zeroed();
                si.cbSize = size_of::<SHSTOCKICONINFO>() as u32;
                // SIID_RECYCLER = 31; SHGSI_ICON = 0x100, SHGSI_SMALLICON = 0x1
                let flags = 0x100 | if large { 0 } else { 0x1 };
                if SHGetStockIconInfo(31, flags, &mut si) != 0 || si.hIcon as usize == 0 {
                    return None;
                }
                let img = icon_to_rgba(si.hIcon);
                DestroyIcon(si.hIcon);
                return img;
            }
        }
        let size = if large { 0 } else { SHGFI_SMALLICON };
        let (path, attrs, flags) = match key {
            Key::Dir => (wide(OsStr::new("folder")), ATTR_DIRECTORY, SHGFI_ICON | size | SHGFI_USEFILEATTRIBUTES),
            Key::Ext(e) => {
                let name = if e.is_empty() { "file".to_string() } else { format!("file.{e}") };
                (wide(OsStr::new(&name)), ATTR_NORMAL, SHGFI_ICON | size | SHGFI_USEFILEATTRIBUTES)
            }
            Key::Path(p) => (wide(p.as_os_str()), 0, SHGFI_ICON | size),
            Key::Recycle => unreachable!(),
        };
        unsafe {
            let mut fi: SHFILEINFOW = zeroed();
            let r = SHGetFileInfoW(path.as_ptr(), attrs, &mut fi, size_of::<SHFILEINFOW>() as u32, flags);
            if r == 0 || fi.hIcon as usize == 0 {
                return None;
            }
            let img = icon_to_rgba(fi.hIcon);
            DestroyIcon(fi.hIcon);
            img
        }
    }

    unsafe fn dib(hdc: HDC, hbm: HBITMAP, w: i32, h: i32) -> Vec<u8> {
        let mut bi: BITMAPINFO = zeroed();
        bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h; // top-down
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = 0; // BI_RGB
        let mut px = vec![0u8; (w * h * 4) as usize];
        GetDIBits(hdc, hbm, 0, h as u32, px.as_mut_ptr() as *mut c_void, &mut bi, 0);
        px
    }

    unsafe fn icon_to_rgba(hicon: windows_sys::Win32::UI::WindowsAndMessaging::HICON) -> Option<(usize, usize, Vec<u8>)> {
        let mut ii: ICONINFO = zeroed();
        if GetIconInfo(hicon, &mut ii) == 0 {
            return None;
        }
        let result = (|| {
            if ii.hbmColor as usize == 0 {
                return None;
            }
            let mut bm: BITMAP = zeroed();
            GetObjectW(ii.hbmColor, size_of::<BITMAP>() as i32, &mut bm as *mut _ as *mut c_void);
            let (w, h) = (bm.bmWidth, bm.bmHeight);
            if w <= 0 || h <= 0 {
                return None;
            }
            let hdc = GetDC(zeroed());
            let mut px = dib(hdc, ii.hbmColor, w, h);
            if !px.chunks_exact(4).any(|c| c[3] != 0) {
                // classic icon without an alpha channel: derive alpha from the AND mask
                let mask = dib(hdc, ii.hbmMask, w, h);
                for (p, m) in px.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                    p[3] = if m[0] == 0 { 255 } else { 0 };
                }
            }
            ReleaseDC(zeroed(), hdc);
            for p in px.chunks_exact_mut(4) {
                p.swap(0, 2); // BGRA -> RGBA
            }
            Some((w as usize, h as usize, px))
        })();
        DeleteObject(ii.hbmColor);
        DeleteObject(ii.hbmMask);
        result
    }
}

#[cfg(not(windows))]
mod shell {
    use super::Key;
    pub fn init() {}
    pub fn load(_: &Key, _: bool) -> Option<(usize, usize, Vec<u8>)> {
        None
    }
}
