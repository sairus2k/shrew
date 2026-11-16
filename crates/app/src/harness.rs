//! What the UI integration tests share: the real views in a headless window on a
//! real core.

use crate::{Root, runtime::tests::init_core};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Bounds, TestAppContext, Window, WindowBounds, WindowOptions,
    px, size,
};
use std::{path::Path, time::Duration};

pub(crate) fn tokio_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap()
}

// What main() does: start the core on the data dir, then open the window
pub(crate) fn launch(
    cx: &mut TestAppContext,
    tokio: &tokio::runtime::Runtime,
    data_dir: &Path,
) -> AnyWindowHandle {
    cx.update(gpui_kit::init);
    let step = init_core(cx, tokio, data_dir);
    // The tokio side is outside GPUI's deterministic scheduler
    cx.executor().allow_parking();
    cx.update(|cx| {
        let bounds = Bounds::from_corners(Default::default(), size(px(800.), px(600.)).into());
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        let (window, _) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Root::new(step, window, cx))
        })
        .unwrap();
        window
    })
}

pub(crate) fn in_window<R>(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    f: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        f(window, cx)
    })
    .unwrap()
}

// The answer comes from a real HTTP request on the tokio runtime, so the
// wait is in real time, not on GPUI's test clock
pub(crate) fn wait_until(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    mut reached: impl FnMut(&mut Window) -> bool,
) {
    for _ in 0..500 {
        cx.run_until_parked();
        if in_window(cx, window, |window, _| reached(window)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the window did not reach the expected state in 5 s");
}
