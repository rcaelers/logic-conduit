use std::cell::RefCell;
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, define_class, msg_send, sel};
use objc2_app_kit::{NSApp, NSImage, NSMenu, NSMenuItem, NSWindow};
use objc2_foundation::{MainThreadMarker, NSObject, ns_string};

use logic_analyzer_ui::{
    NativeMenuCommand, application_input_bindings, dispatch_native_menu_command,
};

thread_local! {
    /// "Open Recent" items dispatch through one shared `openRecent:`
    /// action; each item's `tag` indexes into this list. Kept in a
    /// thread-local (main-thread only, like everything else here) so
    /// `refresh_recent_files` can update it in place as files are
    /// opened/saved during the session, instead of it only ever
    /// reflecting what was persisted as of the last launch.
    static RECENT_FILES: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    /// The live "Open Recent" `NSMenu` and its items' target, kept
    /// around so `refresh_recent_files` can rebuild the submenu in
    /// place rather than only being able to set it once at `install()`.
    static RECENT_MENU: RefCell<Option<(Retained<NSMenu>, Retained<MenuHandler>)>> =
        const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "LogicConduitMenuHandler"]
    struct MenuHandler;

    impl MenuHandler {
        #[unsafe(method(init))]
        fn init(&self) -> *mut Self {
            unsafe { msg_send![super(self), init] }
        }

        #[unsafe(method(showAbout:))]
        fn show_about(&self, _sender: &AnyObject) {
            dispatch_native_menu_command(NativeMenuCommand::About);
        }

        #[unsafe(method(newGraph:))]
        fn new_graph(&self, _sender: &AnyObject) {
            dispatch_native_menu_command(NativeMenuCommand::New);
        }

        #[unsafe(method(loadGraph:))]
        fn load_graph(&self, _sender: &AnyObject) {
            dispatch_native_menu_command(NativeMenuCommand::Load);
        }

        #[unsafe(method(openRecent:))]
        fn open_recent(&self, sender: &NSMenuItem) {
            let index = sender.tag();
            let path = RECENT_FILES.with(|files| {
                usize::try_from(index)
                    .ok()
                    .and_then(|i| files.borrow().get(i).cloned())
            });
            if let Some(path) = path {
                dispatch_native_menu_command(NativeMenuCommand::LoadPath(path));
            }
        }

        #[unsafe(method(clearRecent:))]
        fn clear_recent(&self, _sender: &AnyObject) {
            dispatch_native_menu_command(NativeMenuCommand::ClearRecent);
        }

        #[unsafe(method(saveGraph:))]
        fn save_graph(&self, _sender: &AnyObject) {
            dispatch_native_menu_command(NativeMenuCommand::Save);
        }

        #[unsafe(method(saveGraphAs:))]
        fn save_graph_as(&self, _sender: &AnyObject) {
            dispatch_native_menu_command(NativeMenuCommand::SaveAs);
        }

        #[unsafe(method(saveCaptureData:))]
        fn save_capture_data(&self, _sender: &AnyObject) {
            dispatch_native_menu_command(NativeMenuCommand::SaveCaptureData);
        }

        #[unsafe(method(quitApplication:))]
        fn quit_application(&self, _sender: &AnyObject) {
            dispatch_native_menu_command(NativeMenuCommand::Quit);
        }

        #[unsafe(method(runPipeline:))]
        fn run_pipeline(&self, _sender: &AnyObject) {
            dispatch_native_menu_command(NativeMenuCommand::Run);
        }

        #[unsafe(method(stopPipeline:))]
        fn stop_pipeline(&self, _sender: &AnyObject) {
            dispatch_native_menu_command(NativeMenuCommand::Stop);
        }

        #[unsafe(method(clearDerivedCaches:))]
        fn clear_derived_caches(&self, _sender: &AnyObject) {
            dispatch_native_menu_command(NativeMenuCommand::ClearDerivedCaches);
        }

        #[unsafe(method(showWatches:))]
        fn show_watches(&self, _sender: &AnyObject) {
            dispatch_native_menu_command(NativeMenuCommand::ShowWatches);
        }

        #[unsafe(method(showTriggers:))]
        fn show_triggers(&self, _sender: &AnyObject) {
            dispatch_native_menu_command(NativeMenuCommand::ShowTriggers);
        }

        #[unsafe(method(showDecoder:))]
        fn show_decoder(&self, _sender: &AnyObject) {
            dispatch_native_menu_command(NativeMenuCommand::ShowDecoder);
        }

        #[unsafe(method(resetLayout:))]
        fn reset_layout(&self, _sender: &AnyObject) {
            dispatch_native_menu_command(NativeMenuCommand::ResetLayout);
        }
    }
);

fn make_handler() -> Retained<MenuHandler> {
    unsafe {
        let object: *mut MenuHandler = msg_send![MenuHandler::class(), alloc];
        Retained::from_raw(msg_send![object, init]).expect("failed to create menu handler")
    }
}

fn shortcut(action: &str) -> Retained<objc2_foundation::NSString> {
    let shortcut = application_input_bindings()
        .shortcut(&["global"], action)
        .unwrap_or_else(|| panic!("missing global.{action} input binding"));
    let mut key = shortcut.logical_key.name().to_ascii_lowercase();
    if shortcut.modifiers.shift {
        key.make_ascii_uppercase();
    }
    objc2_foundation::NSString::from_str(&key)
}

unsafe fn menu_item(
    mtm: MainThreadMarker,
    title: &objc2_foundation::NSString,
    action: objc2::runtime::Sel,
    shortcut: &objc2_foundation::NSString,
    handler: &MenuHandler,
) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            title,
            Some(action),
            shortcut,
        )
    };
    unsafe { item.setTarget(Some(handler as &AnyObject)) };
    item
}

unsafe fn menu_item_with_symbol(
    mtm: MainThreadMarker,
    title: &objc2_foundation::NSString,
    action: objc2::runtime::Sel,
    symbol_name: &objc2_foundation::NSString,
    handler: &MenuHandler,
) -> Retained<NSMenuItem> {
    let item = unsafe { menu_item(mtm, title, action, ns_string!(""), handler) };
    if let Some(image) =
        NSImage::imageWithSystemSymbolName_accessibilityDescription(symbol_name, Some(title))
    {
        item.setImage(Some(&image));
    }
    item
}

/// Rebuilds `menu`'s items in place from `paths` (existing files only),
/// each tagged with its index into `paths` for `openRecent:` to resolve,
/// and updates `RECENT_FILES` to match. Used both for the initial build
/// at `install()` time and by `refresh_recent_files` to keep the
/// submenu live as files are opened/saved during the session.
fn populate_recent_menu(
    mtm: MainThreadMarker,
    menu: &NSMenu,
    handler: &MenuHandler,
    paths: &[PathBuf],
) {
    menu.removeAllItems();
    let mut any_files = false;
    for (index, path) in paths.iter().enumerate() {
        if !path.exists() {
            continue;
        }
        any_files = true;
        let title = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("?");
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &objc2_foundation::NSString::from_str(title),
                Some(sel!(openRecent:)),
                ns_string!(""),
            )
        };
        unsafe {
            item.setTarget(Some(handler as &AnyObject));
            item.setTag(index as isize);
        }
        menu.addItem(&item);
    }
    if !any_files {
        let empty = NSMenuItem::new(mtm);
        empty.setTitle(ns_string!("No Recent Files"));
        empty.setEnabled(false);
        menu.addItem(&empty);
    }

    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let clear_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            ns_string!("Clear Recent"),
            Some(sel!(clearRecent:)),
            ns_string!(""),
        )
    };
    unsafe { clear_item.setTarget(Some(handler as &AnyObject)) };
    clear_item.setEnabled(any_files);
    menu.addItem(&clear_item);

    RECENT_FILES.with(|files| *files.borrow_mut() = paths.to_vec());
}

/// Rebuilds the native "Open Recent" submenu from the current app
/// state — registered with `logic_analyzer_ui::set_recent_files_listener` so it
/// fires every time the recent-files list changes during the session,
/// not just at startup. A no-op if `install()` hasn't run yet or this
/// somehow gets called off the main thread.
pub(crate) fn refresh_recent_files(paths: &[PathBuf]) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    RECENT_MENU.with(|state| {
        if let Some((menu, handler)) = state.borrow().as_ref() {
            populate_recent_menu(mtm, menu, handler, paths);
        }
    });
}

pub(crate) fn disable_automatic_window_tabbing() {
    let mtm =
        MainThreadMarker::new().expect("must configure window tabbing on the main thread");
    NSWindow::setAllowsAutomaticWindowTabbing(false, mtm);
}

pub(crate) fn install(recent_files: &[PathBuf]) {
    let mtm = MainThreadMarker::new().expect("must install the menu on the main thread");
    let app = NSApp(mtm);
    let Some(menu_bar) = app.mainMenu() else {
        return;
    };
    let handler = make_handler();

    let file_menu_item = NSMenuItem::new(mtm);
    let file_menu = NSMenu::initWithTitle(mtm.alloc(), ns_string!("File"));
    unsafe {
        file_menu.addItem(&menu_item(
            mtm,
            ns_string!("New"),
            sel!(newGraph:),
            &shortcut("new"),
            &handler,
        ));
        file_menu.addItem(&menu_item(
            mtm,
            ns_string!("Load..."),
            sel!(loadGraph:),
            &shortcut("open"),
            &handler,
        ));
        let recent_menu_item = NSMenuItem::new(mtm);
        recent_menu_item.setTitle(ns_string!("Open Recent"));
        let recent_menu = NSMenu::initWithTitle(mtm.alloc(), ns_string!("Open Recent"));
        populate_recent_menu(mtm, &recent_menu, &handler, recent_files);
        recent_menu_item.setSubmenu(Some(&recent_menu));
        file_menu.addItem(&recent_menu_item);
        // Kept alive here (rather than via the `mem::forget` below) so
        // `refresh_recent_files` can find the live menu + target later.
        RECENT_MENU.with(|state| {
            *state.borrow_mut() = Some((recent_menu, Retained::clone(&handler)));
        });
        file_menu.addItem(&menu_item(
            mtm,
            ns_string!("Save"),
            sel!(saveGraph:),
            &shortcut("save"),
            &handler,
        ));
        file_menu.addItem(&menu_item(
            mtm,
            ns_string!("Save As..."),
            sel!(saveGraphAs:),
            &shortcut("save_as"),
            &handler,
        ));
        file_menu.addItem(&NSMenuItem::separatorItem(mtm));
        file_menu.addItem(&menu_item(
            mtm,
            ns_string!("Save Capture Data..."),
            sel!(saveCaptureData:),
            ns_string!(""),
            &handler,
        ));
    }
    file_menu_item.setSubmenu(Some(&file_menu));
    menu_bar.addItem(&file_menu_item);

    let view_menu_item = NSMenuItem::new(mtm);
    let view_menu = NSMenu::initWithTitle(mtm.alloc(), ns_string!("View"));
    unsafe {
        view_menu.addItem(&menu_item_with_symbol(
            mtm,
            ns_string!("Watches"),
            sel!(showWatches:),
            ns_string!("list.bullet"),
            &handler,
        ));
        view_menu.addItem(&menu_item_with_symbol(
            mtm,
            ns_string!("Triggers"),
            sel!(showTriggers:),
            ns_string!("scope"),
            &handler,
        ));
        view_menu.addItem(&menu_item_with_symbol(
            mtm,
            ns_string!("Decoder"),
            sel!(showDecoder:),
            ns_string!("tablecells"),
            &handler,
        ));
        view_menu.addItem(&NSMenuItem::separatorItem(mtm));
        view_menu.addItem(&menu_item_with_symbol(
            mtm,
            ns_string!("Reset Layout"),
            sel!(resetLayout:),
            ns_string!("arrow.counterclockwise"),
            &handler,
        ));
    }
    view_menu_item.setSubmenu(Some(&view_menu));
    menu_bar.addItem(&view_menu_item);

    let pipeline_menu_item = NSMenuItem::new(mtm);
    let pipeline_menu = NSMenu::initWithTitle(mtm.alloc(), ns_string!("Pipeline"));
    unsafe {
        pipeline_menu.addItem(&menu_item(
            mtm,
            ns_string!("Run"),
            sel!(runPipeline:),
            &shortcut("run"),
            &handler,
        ));
        pipeline_menu.addItem(&menu_item(
            mtm,
            ns_string!("Stop"),
            sel!(stopPipeline:),
            &shortcut("stop"),
            &handler,
        ));
        pipeline_menu.addItem(&NSMenuItem::separatorItem(mtm));
        pipeline_menu.addItem(&menu_item(
            mtm,
            ns_string!("Clear All Derived Data Caches..."),
            sel!(clearDerivedCaches:),
            ns_string!(""),
            &handler,
        ));
    }
    pipeline_menu_item.setSubmenu(Some(&pipeline_menu));
    menu_bar.addItem(&pipeline_menu_item);

    if let Some(application_menu) = menu_bar.itemAtIndex(0).and_then(|item| item.submenu()) {
        for index in 0..application_menu.numberOfItems() {
            let Some(item) = application_menu.itemAtIndex(index) else {
                continue;
            };
            if item.keyEquivalent().to_string() == "q" {
                item.setKeyEquivalent(&shortcut("quit"));
                unsafe {
                    item.setTarget(Some(&handler as &AnyObject));
                    item.setAction(Some(sel!(quitApplication:)));
                }
            }
            // Point the standard "About …" item at our in-app window
            // instead of the Cocoa about panel.
            if item.action() == Some(sel!(orderFrontStandardAboutPanel:)) {
                unsafe {
                    item.setTarget(Some(&handler as &AnyObject));
                    item.setAction(Some(sel!(showAbout:)));
                }
            }
        }
    }

    logic_analyzer_ui::set_recent_files_listener(refresh_recent_files);

    // NSMenuItem keeps a weak target, so retain the target for the app
    // lifetime. `RECENT_MENU` already holds a clone, but the original
    // binding still needs this — nothing else owns it.
    std::mem::forget(handler);
}
