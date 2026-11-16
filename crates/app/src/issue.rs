//! One issue as the UI sees it: a copy of what the cache holds that reloads
//! itself whenever the core reports a change to that issue. The views that show
//! the issue share it and repaint when it notifies.

use crate::runtime::{Core, spawn};
use app_core::{
    SyncEvent, api,
    models::{IssueDetail, IssueKey},
};
use gpui_kit::*;

pub struct IssueModel {
    pub key: IssueKey,
    // `None` until the first read, and when the cache does not have the issue
    pub detail: Option<IssueDetail>,
    _load: Task<()>,
    _sync: Subscription,
}

impl IssueModel {
    pub fn new(key: IssueKey, cx: &mut Context<Self>) -> Self {
        let sync = cx.global::<Core>().sync.clone();
        let _sync = cx.subscribe(&sync, |this, _, event: &SyncEvent, cx| {
            if let SyncEvent::IssuesUpdated(keys) = event
                && keys.contains(&this.key)
            {
                this.reload(cx);
            }
        });
        let mut this = Self {
            key,
            detail: None,
            _load: Task::ready(()),
            _sync,
        };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let key = self.key.clone();
        let load = spawn(cx, move |state| async move {
            api::get_issue(&state, &key).await
        });
        // Assigning the task drops the previous one, so an older read never
        // overwrites a newer one
        self._load = cx.spawn(async move |this, cx| {
            let detail = load.await.ok();
            this.update(cx, |this, cx| {
                this.detail = detail;
                cx.notify();
            })
            .ok();
        });
    }
}
