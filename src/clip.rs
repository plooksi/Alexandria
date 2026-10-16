//! System clipboard file list (CF_HDROP + "Preferred DropEffect"), so copy / cut /
//! paste interoperate with Windows Explorer and any other app.
//! Cut is the standard Explorer protocol: nothing is deleted until the paste happens.

use std::path::PathBuf;

#[cfg(windows)]
mod imp {
    use std::{
        ffi::OsString,
        os::windows::ffi::{OsStrExt, OsStringExt},
        path::PathBuf,
        thread,
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::HGLOBAL,
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
                IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
            },
            Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
        },
    };

    const CF_HDROP: u32 = 15;

    fn effect_format() -> u32 {
        let name: Vec<u16> = "Preferred DropEffect".encode_utf16().chain(Some(0)).collect();
        unsafe { RegisterClipboardFormatW(name.as_ptr()) }
    }

    unsafe fn open() -> bool {
        for _ in 0..10 {
            if OpenClipboard(std::mem::zeroed()) != 0 {
                return true;
            }
            thread::sleep(Duration::from_millis(10)); // another app may hold it briefly
        }
        false
    }

    unsafe fn alloc(bytes: &[u8]) -> Option<HGLOBAL> {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        if h as usize == 0 {
            return None;
        }
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        GlobalUnlock(h);
        Some(h)
    }

    pub fn sequence() -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    pub fn set_files(paths: &[PathBuf], cut: bool) -> bool {
        unsafe {
            // DROPFILES header: pFiles=20, pt (8 bytes), fNC=0, fWide=1
            let mut data: Vec<u8> = Vec::new();
            data.extend_from_slice(&20u32.to_le_bytes());
            data.extend_from_slice(&[0u8; 8]);
            data.extend_from_slice(&0i32.to_le_bytes());
            data.extend_from_slice(&1i32.to_le_bytes());
            for p in paths {
                for u in p.as_os_str().encode_wide() {
                    data.extend_from_slice(&u.to_le_bytes());
                }
                data.extend_from_slice(&[0, 0]);
            }
            data.extend_from_slice(&[0, 0]);

            let effect: u32 = if cut { 2 } else { 1 }; // DROPEFFECT_MOVE / DROPEFFECT_COPY
            let (Some(h_files), Some(h_effect)) = (alloc(&data), alloc(&effect.to_le_bytes())) else {
                return false;
            };
            if !open() {
                return false;
            }
            EmptyClipboard();
            let ok = SetClipboardData(CF_HDROP, h_files as _) as usize != 0;
            SetClipboardData(effect_format(), h_effect as _);
            CloseClipboard();
            ok
        }
    }

    fn parse(bytes: &[u8]) -> Option<Vec<PathBuf>> {
        if bytes.len() < 20 {
            return None;
        }
        let off = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
        let wide = i32::from_le_bytes(bytes[16..20].try_into().ok()?) != 0;
        let body = bytes.get(off..)?;
        let mut out = Vec::new();
        if wide {
            let u: Vec<u16> = body.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            for part in u.split(|&c| c == 0) {
                if part.is_empty() {
                    break;
                }
                out.push(PathBuf::from(OsString::from_wide(part)));
            }
        } else {
            for part in body.split(|&c| c == 0) {
                if part.is_empty() {
                    break;
                }
                out.push(PathBuf::from(String::from_utf8_lossy(part).into_owned()));
            }
        }
        Some(out)
    }

    pub fn get_files() -> Option<(Vec<PathBuf>, bool)> {
        unsafe {
            if IsClipboardFormatAvailable(CF_HDROP) == 0 || !open() {
                return None;
            }
            let mut files = None;
            let h = GetClipboardData(CF_HDROP);
            if h as usize != 0 {
                let p = GlobalLock(h) as *const u8;
                if !p.is_null() {
                    let size = GlobalSize(h);
                    files = parse(std::slice::from_raw_parts(p, size));
                    GlobalUnlock(h);
                }
            }
            let mut cut = false;
            let eh = GetClipboardData(effect_format());
            if eh as usize != 0 {
                let p = GlobalLock(eh) as *const u8;
                if !p.is_null() && GlobalSize(eh) >= 4 {
                    let v = u32::from_le_bytes(std::slice::from_raw_parts(p, 4).try_into().unwrap());
                    cut = v & 2 != 0;
                }
                GlobalUnlock(eh);
            }
            CloseClipboard();
            files.map(|f| (f, cut))
        }
    }

    pub fn clear() {
        unsafe {
            if open() {
                EmptyClipboard();
                CloseClipboard();
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::PathBuf;
    pub fn sequence() -> u32 {
        0
    }
    pub fn set_files(_: &[PathBuf], _: bool) -> bool {
        false
    }
    pub fn get_files() -> Option<(Vec<PathBuf>, bool)> {
        None
    }
    pub fn clear() {}
}

/// Changes whenever any app changes the clipboard (cheap to poll every frame).
pub fn sequence() -> u32 {
    imp::sequence()
}
/// Puts a file list on the system clipboard (`cut` = Explorer "move" semantics).
pub fn set_files(paths: &[PathBuf], cut: bool) -> bool {
    imp::set_files(paths, cut)
}
/// Reads the file list currently on the system clipboard, plus whether it was cut.
pub fn get_files() -> Option<(Vec<PathBuf>, bool)> {
    imp::get_files()
}
pub fn clear() {
    imp::clear()
}
