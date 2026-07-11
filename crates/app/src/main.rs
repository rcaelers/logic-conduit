#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(target_arch = "wasm32"))]
use clap::Parser;

#[cfg(target_os = "macos")]
mod macos_menu {
    use dsl_ui::{NativeMenuCommand, dispatch_native_menu_command};
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{ClassType, define_class, msg_send, sel};
    use objc2_app_kit::{NSApp, NSMenu, NSMenuItem};
    use objc2_foundation::{MainThreadMarker, NSObject, ns_string};

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "DslPipelineMenuHandler"]
        struct MenuHandler;

        impl MenuHandler {
            #[unsafe(method(init))]
            fn init(&self) -> *mut Self {
                unsafe { msg_send![super(self), init] }
            }

            #[unsafe(method(loadGraph:))]
            fn load_graph(&self, _sender: &AnyObject) {
                dispatch_native_menu_command(NativeMenuCommand::Load);
            }

            #[unsafe(method(saveGraph:))]
            fn save_graph(&self, _sender: &AnyObject) {
                dispatch_native_menu_command(NativeMenuCommand::Save);
            }

            #[unsafe(method(saveGraphAs:))]
            fn save_graph_as(&self, _sender: &AnyObject) {
                dispatch_native_menu_command(NativeMenuCommand::SaveAs);
            }

            #[unsafe(method(quitApplication:))]
            fn quit_application(&self, _sender: &AnyObject) {
                dispatch_native_menu_command(NativeMenuCommand::Quit);
            }
        }
    );

    fn make_handler() -> Retained<MenuHandler> {
        unsafe {
            let object: *mut MenuHandler = msg_send![MenuHandler::class(), alloc];
            Retained::from_raw(msg_send![object, init]).expect("failed to create menu handler")
        }
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

    pub fn install() {
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
                ns_string!("Load..."),
                sel!(loadGraph:),
                ns_string!("o"),
                &handler,
            ));
            file_menu.addItem(&menu_item(
                mtm,
                ns_string!("Save"),
                sel!(saveGraph:),
                ns_string!("s"),
                &handler,
            ));
            file_menu.addItem(&menu_item(
                mtm,
                ns_string!("Save As..."),
                sel!(saveGraphAs:),
                ns_string!("S"),
                &handler,
            ));
        }
        file_menu_item.setSubmenu(Some(&file_menu));
        menu_bar.addItem(&file_menu_item);

        if let Some(application_menu) = menu_bar.itemAtIndex(0).and_then(|item| item.submenu()) {
            for index in 0..application_menu.numberOfItems() {
                let Some(item) = application_menu.itemAtIndex(index) else {
                    continue;
                };
                if item.keyEquivalent().to_string() == "q" {
                    unsafe {
                        item.setTarget(Some(&handler as &AnyObject));
                        item.setAction(Some(sel!(quitApplication:)));
                    }
                }
            }
        }

        // NSMenuItem keeps a weak target, so retain the target for the app lifetime.
        std::mem::forget(handler);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Parser)]
#[command(version, about = "DSL Pipeline Editor")]
struct Args {
    /// Graph JSON file to load at startup
    file: Option<std::path::PathBuf>,
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([2100.0, 1350.0])
            .with_title("DSL Pipeline Editor"),
        ..Default::default()
    };
    eframe::run_native(
        "DSL Pipeline Editor",
        options,
        Box::new(move |cc| {
            let app = dsl_ui::App::new_with_plugins_and_file(cc, args.file.as_deref(), |_ctx| {
                #[cfg(feature = "example-plugin")]
                example_plugin::register(_ctx);
            });
            #[cfg(target_os = "macos")]
            macos_menu::install();
            Ok(Box::new(app))
        }),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn accepts_optional_startup_file() {
        let empty = Args::try_parse_from(["dsl-ui"]).unwrap();
        assert!(empty.file.is_none());

        let with_file = Args::try_parse_from(["dsl-ui", "examples/ccd_pipeline.json"]).unwrap();
        assert_eq!(
            with_file.file.as_deref(),
            Some(std::path::Path::new("examples/ccd_pipeline.json"))
        );
    }
}
