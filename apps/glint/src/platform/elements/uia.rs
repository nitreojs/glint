use glint_core::geom::rect::Rect;
use glint_media::rect::WinRect;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationCacheRequest, IUIAutomationElement,
    TreeScope, TreeScope_Children, TreeScope_Element, UIA_BoundingRectanglePropertyId, UIA_E_TIMEOUT,
    UIA_IsOffscreenPropertyId,
};
use windows::core::Interface;

/// How long UIA waits for a provider to connect / answer before failing with a timeout.
const CONNECT_MS: u32 = 1000;
const TRANSACT_MS: u32 = 1000;

pub(super) type Element = IUIAutomationElement;
/// Children with their bounds, in screen px.
pub(super) type Kids = Vec<(Rect, Element)>;

pub(super) enum Failed {
    /// The app didn't answer in time.
    Timeout,
    /// The element is gone or has no provider.
    Gone,
}

/// A UI Automation client whose every fetch brings one element's bounds and its on-screen
/// control-view children in a single round trip.
pub(super) struct Uia {
    auto: IUIAutomation,
    req: IUIAutomationCacheRequest,
}

impl Uia {
    /// Needs COM initialized on the calling thread.
    pub(super) fn new() -> Option<Uia> {
        unsafe {
            let auto: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
                .or_else(|_| CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER))
                .ok()?;
            if let Ok(a2) = auto.cast::<IUIAutomation2>() {
                let _ = a2.SetConnectionTimeout(CONNECT_MS);
                let _ = a2.SetTransactionTimeout(TRANSACT_MS);
            }
            let req = auto.CreateCacheRequest().ok()?;
            req.AddProperty(UIA_BoundingRectanglePropertyId).ok()?;
            req.AddProperty(UIA_IsOffscreenPropertyId).ok()?;
            req.SetTreeScope(TreeScope(TreeScope_Element.0 | TreeScope_Children.0)).ok()?;
            req.SetTreeFilter(&auto.ControlViewCondition().ok()?).ok()?;
            Some(Uia { auto, req })
        }
    }

    /// The window's element, its bounds and its children.
    pub(super) fn root(&self, hwnd: HWND) -> Result<(Element, Rect, Kids), Failed> {
        unsafe {
            let el = self.auto.ElementFromHandleBuildCache(hwnd, &self.req).map_err(failed)?;
            let rect = Rect::from_win(&el.CachedBoundingRectangle().map_err(failed)?);
            let kids = kids_of(&el);
            Ok((el, rect, kids))
        }
    }

    /// The children of `el` with their bounds, minus offscreen ones. Those without bounds stay:
    /// containers like XAML islands report none yet hold the visible content.
    pub(super) fn kids(&self, el: &Element) -> Result<Kids, Failed> {
        let el = unsafe { el.BuildUpdatedCache(&self.req) }.map_err(failed)?;
        Ok(kids_of(&el))
    }
}

fn kids_of(el: &Element) -> Kids {
    unsafe {
        let Ok(arr) = el.GetCachedChildren() else { return Vec::new() };
        (0..arr.Length().unwrap_or(0))
            .filter_map(|i| {
                let e = arr.GetElement(i).ok()?;
                if e.CachedIsOffscreen().ok()?.as_bool() {
                    return None;
                }
                let r = Rect::from_win(&e.CachedBoundingRectangle().ok()?);
                Some((r, e))
            })
            .collect()
    }
}

fn failed(e: windows::core::Error) -> Failed {
    if e.code().0 as u32 == UIA_E_TIMEOUT { Failed::Timeout } else { Failed::Gone }
}
