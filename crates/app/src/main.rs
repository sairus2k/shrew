use gpui_kit::*;
use runtime::{Core, SyncBridge};
use sign_in::{SignIn, SignedIn};

mod runtime;
mod sign_in;

// Names the data directory: ~/Library/Application Support/<APP_ID>
const APP_ID: &str = "dev.shrew.app";

struct Root {
    // `Some` until the app has credentials
    sign_in: Option<Entity<SignIn>>,
}

impl Root {
    fn new(signed_in: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sign_in = (!signed_in).then(|| {
            let sign_in = cx.new(|cx| SignIn::new(window, cx));
            cx.subscribe(&sign_in, |this: &mut Self, _, _: &SignedIn, cx| {
                this.sign_in = None;
                cx.notify();
            })
            .detach();
            sign_in
        });
        Self { sign_in }
    }
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if let Some(sign_in) = &self.sign_in {
            return sign_in.clone().into_any_element();
        }
        div()
            .id("home")
            .test_support()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child("Shrew")
            .into_any_element()
    }
}

fn main() -> anyhow::Result<()> {
    // sqlx and reqwest need a tokio runtime. GPUI drives the UI on its own executor,
    // so tokio runs beside it on two worker threads.
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let handle = tokio.handle().clone();

    let data_dir = dirs::data_dir().expect("no data dir").join(APP_ID);
    let (state, events) = tokio.block_on(app_core::start(&data_dir))?;
    let signed_in = tokio.block_on(app_core::api::account(&state)).is_some();

    application().with_assets(assets::Assets).run(move |cx| {
        init(cx);

        let sync = cx.new(|cx| SyncBridge::new(events, cx));
        cx.set_global(Core {
            state,
            tokio: handle,
            sync,
        });

        open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Root::new(signed_in, window, cx))
        })
        .expect("failed to open window");
        cx.activate(true);
    });
    Ok(())
}
