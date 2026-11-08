//! The PRIMARY selection of X11 and Wayland: selecting text copies it there and a
//! middle click pastes it, separately from the clipboard. Other platforms have no
//! PRIMARY selection, so there this is a no-op.
use raw_window_handle::RawDisplayHandle;

pub struct Primary {
    inner: Inner,
}

enum Inner {
    #[cfg(target_os = "linux")]
    Wayland(smithay_clipboard::Clipboard),
    #[cfg(target_os = "linux")]
    X11(arboard::Clipboard),
    None,
}

impl Primary {
    /// A PRIMARY selection that stores and returns nothing.
    pub fn none() -> Self {
        Self { inner: Inner::None }
    }

    /// The PRIMARY selection of the display the window is on.
    ///
    /// # Safety
    ///
    /// For Wayland, `display` must stay valid for as long as the returned value
    /// lives. eframe's display connection outlives the app, which owns this.
    pub unsafe fn new(display: Option<RawDisplayHandle>) -> Self {
        #[cfg(target_os = "linux")]
        {
            match display {
                Some(RawDisplayHandle::Wayland(handle)) => {
                    // SAFETY: the caller guarantees the display outlives the clipboard.
                    let clipboard =
                        unsafe { smithay_clipboard::Clipboard::new(handle.display.as_ptr()) };
                    return Self {
                        inner: Inner::Wayland(clipboard),
                    };
                }
                Some(RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_)) => {
                    if let Ok(clipboard) = arboard::Clipboard::new() {
                        return Self {
                            inner: Inner::X11(clipboard),
                        };
                    }
                }
                _ => {}
            }
        }
        let _ = display;
        Self::none()
    }

    pub fn is_available(&self) -> bool {
        !matches!(self.inner, Inner::None)
    }

    pub fn set(&mut self, text: &str) {
        match &mut self.inner {
            #[cfg(target_os = "linux")]
            Inner::Wayland(clipboard) => clipboard.store_primary(text),
            #[cfg(target_os = "linux")]
            Inner::X11(clipboard) => {
                use arboard::{LinuxClipboardKind, SetExtLinux};
                let _ = clipboard
                    .set()
                    .clipboard(LinuxClipboardKind::Primary)
                    .text(text);
            }
            Inner::None => {
                let _ = text;
            }
        }
    }

    pub fn get(&mut self) -> Option<String> {
        let text: Option<String> = match &mut self.inner {
            #[cfg(target_os = "linux")]
            Inner::Wayland(clipboard) => clipboard.load_primary().ok(),
            #[cfg(target_os = "linux")]
            Inner::X11(clipboard) => {
                use arboard::{GetExtLinux, LinuxClipboardKind};
                clipboard
                    .get()
                    .clipboard(LinuxClipboardKind::Primary)
                    .text()
                    .ok()
            }
            Inner::None => None,
        };
        text.filter(|t| !t.is_empty())
    }
}
