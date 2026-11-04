//! Dragging files onto the notch and back out of it, and file icons for the Shelf.
//!
//! OLE calls the drop target on the UI thread from inside its own drag loop, so
//! the target never touches app state: dropped paths go into a shared list and a
//! message is posted. Dragging out runs `DoDragDrop`, which is modal, so the
//! caller must hold no borrow of the app across it.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, POINTL, S_OK,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeMedianCut,
};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, DVASPECT_CONTENT, FORMATETC,
    IDataObject, TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DROPEFFECT_NONE,
    DoDragDrop, IDropSource, IDropSource_Impl, IDropTarget, IDropTarget_Impl, ReleaseStgMedium,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    DragQueryFileW, HDROP, SHCreateDataObject, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON,
    SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, SHParseDisplayName,
};
use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;
use windows::core::{BOOL, HRESULT, HSTRING, Ref, Result, implement};

use crate::msg::{self, WindowHandle};

/// Paths dropped on the notch, waiting for the app to pick them up.
pub type Dropped = Rc<RefCell<Vec<PathBuf>>>;

fn hdrop_format() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

/// An entry that is empty or not absolute can't be a real dropped file; it is skipped.
fn is_droppable(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.is_absolute()
}

/// File paths in a drag, or nothing when it carries no files (text, a link).
fn files_in(data: &IDataObject) -> Vec<PathBuf> {
    let mut files = Vec::new();
    unsafe {
        let Ok(mut medium) = data.GetData(&hdrop_format()) else {
            return files;
        };
        // A source may answer with another kind of storage; only a global handle is a drop list.
        if medium.tymed != TYMED_HGLOBAL.0 as u32 {
            ReleaseStgMedium(&mut medium);
            return files;
        }
        let drop = HDROP(medium.u.hGlobal.0);
        // The shelf keeps a dozen files at most, so a drag of thousands reads no more.
        let count = DragQueryFileW(drop, u32::MAX, None).min(crate::shelf::MAX_ITEMS as u32);
        for index in 0..count {
            let len = DragQueryFileW(drop, index, None) as usize;
            let mut buffer = vec![0u16; len + 1];
            let copied = DragQueryFileW(drop, index, Some(&mut buffer)) as usize;
            let path = PathBuf::from(String::from_utf16_lossy(&buffer[..copied]));
            if is_droppable(&path) {
                files.push(path);
            }
        }
        ReleaseStgMedium(&mut medium);
    }
    files
}

#[implement(IDropTarget)]
pub struct DropTarget {
    window: WindowHandle,
    dropped: Dropped,
    /// Whether the drag in progress carries files.
    accepts: Cell<bool>,
}

impl DropTarget {
    pub fn create(window: WindowHandle, dropped: Dropped) -> IDropTarget {
        Self {
            window,
            dropped,
            accepts: Cell::new(false),
        }
        .into()
    }
}

/// The shelf keeps a reference to the file; the source must not delete it.
fn set_effect(effect: *mut DROPEFFECT, accepts: bool) {
    if let Some(effect) = unsafe { effect.as_mut() } {
        *effect = if accepts {
            DROPEFFECT_COPY
        } else {
            DROPEFFECT_NONE
        };
    }
}

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _point: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        let accepts = data
            .ok()
            .is_ok_and(|data| unsafe { data.QueryGetData(&hdrop_format()) } == S_OK);
        self.accepts.set(accepts);
        set_effect(effect, accepts);
        if accepts {
            self.window.post(msg::DRAG_OVER, 1, 0);
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _keys: MODIFIERKEYS_FLAGS,
        _point: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        set_effect(effect, self.accepts.get());
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        if self.accepts.replace(false) {
            self.window.post(msg::DRAG_OVER, 0, 0);
        }
        Ok(())
    }

    fn Drop(
        &self,
        data: Ref<IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _point: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        let files = data.ok().map(files_in).unwrap_or_default();
        set_effect(effect, !files.is_empty());
        self.accepts.set(false);
        if !files.is_empty() {
            self.dropped.borrow_mut().extend(files);
            self.window.post(msg::SHELF_DROP, 0, 0);
        } else {
            self.window.post(msg::DRAG_OVER, 0, 0);
        }
        Ok(())
    }
}

#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Drags `path` out to wherever the user drops it. Modal until the drag ends.
/// Returns the effect the drop target chose (move, copy, link) or `None`.
pub fn drag_out(path: &Path) -> Option<DROPEFFECT> {
    unsafe {
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        SHParseDisplayName(&HSTRING::from(path.as_os_str()), None, &mut pidl, 0, None).ok()?;
        let data: Result<IDataObject> = SHCreateDataObject(None, Some(&[pidl.cast_const()]), None);
        let result = data.ok().and_then(|data| {
            let source: IDropSource = DropSource.into();
            let mut effect = DROPEFFECT_NONE;
            let allowed = DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK;
            (DoDragDrop(&data, &source, allowed, &mut effect) == DRAGDROP_S_DROP).then_some(effect)
        });
        CoTaskMemFree(Some(pidl.cast_const().cast()));
        result
    }
}

/// A file's icon as it appears in Explorer, as 32 x 32 premultiplied BGRA pixels.
/// Files not plainly on a local drive get the generic icon for their type without
/// touching the network, which may be slow or offline.
pub fn file_icon(path: &Path) -> Option<(u32, Vec<u8>)> {
    let mut flags = SHGFI_ICON | SHGFI_LARGEICON;
    if !crate::shelf::is_local(path) {
        flags |= SHGFI_USEFILEATTRIBUTES;
    }
    unsafe {
        let mut info = SHFILEINFOW::default();
        let found = SHGetFileInfoW(
            &HSTRING::from(path.as_os_str()),
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        );
        if found == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let pixels = icon_pixels(info.hIcon);
        let _ = DestroyIcon(info.hIcon);
        pixels
    }
}

unsafe fn icon_pixels(
    icon: windows::Win32::UI::WindowsAndMessaging::HICON,
) -> Option<(u32, Vec<u8>)> {
    unsafe {
        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let bitmap = wic.CreateBitmapFromHICON(icon).ok()?;
        let (mut width, mut height) = (0, 0);
        bitmap.GetSize(&mut width, &mut height).ok()?;
        if width == 0 || width != height || width > 256 {
            return None;
        }
        let converter = wic.CreateFormatConverter().ok()?;
        converter
            .Initialize(
                &bitmap,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeMedianCut,
            )
            .ok()?;
        let mut pixels = vec![0u8; (width * width * 4) as usize];
        converter
            .CopyPixels(std::ptr::null(), width * 4, &mut pixels)
            .ok()?;
        Some((width, pixels))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_droppable_should_skip_empty_and_relative_paths() {
        let checks = [
            "",
            "a.txt",
            r"sub\a.txt",
            r"C:\a.txt",
            r"\\server\share\a.txt",
        ]
        .map(|text| is_droppable(Path::new(text)));
        assert_eq!(checks, [false, false, false, true, true]);
    }
}
