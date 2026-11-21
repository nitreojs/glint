use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;
use windows::core::Interface;

use super::*;

#[test]
fn releasing_back_on_the_source_cancels_but_leaving_it_allows_a_drop() {
    let owner = unsafe { GetDesktopWindow() };
    let source: IDropSource = Source { owner: Some(owner), over_owner: Cell::new(false) }.into();
    let notify: IDropSourceNotify = source.cast().unwrap();
    let up = MODIFIERKEYS_FLAGS::default();

    unsafe {
        notify.DragEnterTarget(owner).unwrap();
        let holding = source.QueryContinueDrag(false, MK_LBUTTON);
        let release_here = source.QueryContinueDrag(false, up);
        notify.DragLeaveTarget().unwrap();
        let release_away = source.QueryContinueDrag(false, up);
        let escape = source.QueryContinueDrag(true, MK_LBUTTON);
        let right = source.QueryContinueDrag(false, MK_RBUTTON | MK_LBUTTON);

        assert_eq!(holding, S_OK);
        assert_eq!(release_here, DRAGDROP_S_CANCEL);
        assert_eq!(release_away, DRAGDROP_S_DROP);
        assert_eq!((escape, right), (DRAGDROP_S_CANCEL, DRAGDROP_S_CANCEL));
    }
}
