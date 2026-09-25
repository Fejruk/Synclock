//! Native left/right click handling for the menu bar icon.
//!
//! `tray-icon` keeps the context menu permanently attached to the status item
//! and catches clicks with a transparent subview laid over the button. On
//! macOS 27 the left click stopped opening the panel that way. This uses the
//! pattern system status items use instead:
//! the button reports left and right mouse-up as its action, left opens the
//! panel, and the menu is attached only for the duration of a right click.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{NSApplication, NSEventMask, NSEventModifierFlags, NSEventType, NSMenu, NSStatusItem};
use objc2_foundation::NSObject;
use std::sync::OnceLock;

/// Called on left click with the icon's horizontal center in physical pixels.
type LeftClick = Box<dyn Fn(f64) + Send + Sync>;
static ON_LEFT_CLICK: OnceLock<LeftClick> = OnceLock::new();

struct Ivars {
    status_item: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SynclockTrayClickHandler"]
    #[ivars = Ivars]
    struct Handler;

    impl Handler {
        #[unsafe(method(onClick:))]
        fn on_click(&self, _sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            let is_menu_click = NSApplication::sharedApplication(mtm)
                .currentEvent()
                .is_some_and(|e| {
                    e.r#type() == NSEventType::RightMouseUp
                        || e.modifierFlags().contains(NSEventModifierFlags::Control)
                });
            let ivars = self.ivars();
            let Some(button) = ivars.status_item.button(mtm) else { return };

            if is_menu_click {
                // performClick opens the attached menu and returns once it
                // closes, so the menu is detached again right after.
                ivars.status_item.setMenu(Some(&ivars.menu));
                unsafe { button.performClick(None) };
                ivars.status_item.setMenu(None);
            } else if let (Some(window), Some(cb)) = (button.window(), ON_LEFT_CLICK.get()) {
                let frame = window.frame();
                cb((frame.origin.x + frame.size.width / 2.0) * window.backingScaleFactor());
            }
        }
    }
);

/// Take over click handling of `status_item`. The status item must already
/// carry the context menu (set through Tauri); it is detached and shown on
/// right click only.
pub fn install(status_item: &NSStatusItem, on_left_click: impl Fn(f64) + Send + Sync + 'static) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(menu) = status_item.menu(mtm) else { return };
    let Some(button) = status_item.button(mtm) else { return };
    let _ = ON_LEFT_CLICK.set(Box::new(on_left_click));

    // Hide tray-icon's click-catching overlay so the button gets the clicks.
    for view in button.subviews() {
        if view.class().name().to_bytes() == b"TaoTrayTarget" {
            view.setHidden(true);
        }
    }
    status_item.setMenu(None);

    let handler = Handler::alloc(mtm).set_ivars(Ivars { status_item: status_item.retain(), menu });
    let handler: Retained<Handler> = unsafe { msg_send![super(handler), init] };
    unsafe {
        button.setTarget(Some(&handler));
        button.setAction(Some(sel!(onClick:)));
    }
    button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
    // The button holds its target weakly; the handler lives as long as the app.
    std::mem::forget(handler);
}
