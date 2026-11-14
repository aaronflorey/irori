//! RomM's sign-in, drawn by Irori: the user names their server, Irori opens
//! RomM's approval page in the browser, and polls until RomM issues the
//! token. Polling stops when the view is dropped (the dialog closed).

use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Task, Window, div,
};
use irori_romm::signin::{DeviceError, Poll, SignIn};
use irori_ui::i18n::t;

/// How much longer to wait after RomM asks to poll more slowly.
const SLOW_DOWN: Duration = Duration::from_secs(5);

/// RomM approved the device: what [`irori_core::Store::login`] takes.
pub(crate) struct RommSignedIn(pub String);

pub(crate) struct RommSignInView {
    server: Entity<InputState>,
    /// The sign-in waiting for approval in the browser.
    signin: Option<SignIn>,
    starting: bool,
    error: Option<SharedString>,
    /// Starts the sign-in, then polls it; replacing it stops the previous one.
    task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<RommSignedIn> for RommSignInView {}

impl RommSignInView {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let server =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://romm.example.com"));
        let subscriptions = vec![
            cx.subscribe_in(&server, window, |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.start(window, cx),
                InputEvent::Change => {
                    this.error = None;
                    cx.notify();
                }
                _ => {}
            }),
        ];
        server.read(cx).focus_handle(cx).focus(window, cx);
        Self {
            server,
            signin: None,
            starting: false,
            error: None,
            task: None,
            _subscriptions: subscriptions,
        }
    }

    /// Ask the server for a code, open its approval page and wait.
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let server = self.server.read(cx).value().trim().to_string();
        if self.starting || server.is_empty() {
            return;
        }
        self.starting = true;
        self.error = None;
        cx.notify();
        let started = irori_ui::run(cx, async move { SignIn::start(&server).await });
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let started = started.await;
            let signin = this.update(cx, |this, cx| {
                this.starting = false;
                cx.notify();
                match started {
                    Ok(signin) => {
                        cx.open_url(signin.url());
                        this.signin = Some(signin.clone());
                        Some(signin)
                    }
                    Err(e) => {
                        this.error = Some(error_text(&e));
                        None
                    }
                }
            });
            let Ok(Some(signin)) = signin else { return };
            let mut interval = signin.interval();
            loop {
                cx.background_executor().timer(interval).await;
                let poll = signin.clone();
                let Ok(task) = this.update(cx, |_, cx| {
                    irori_ui::run(cx, async move { poll.poll().await })
                }) else {
                    return;
                };
                let polled = task.await;
                let done = this.update(cx, |this, cx| {
                    let done = match polled {
                        Ok(Poll::Pending) => false,
                        Ok(Poll::SlowDown) => {
                            interval += SLOW_DOWN;
                            false
                        }
                        Ok(Poll::Approved(token)) => {
                            cx.emit(RommSignedIn(signin.login_input(&token.access_token)));
                            true
                        }
                        Err(e) => {
                            this.signin = None;
                            this.error = Some(error_text(&e));
                            true
                        }
                    };
                    cx.notify();
                    done
                });
                if !matches!(done, Ok(false)) {
                    return;
                }
            }
        }));
    }

    /// Stop waiting and let the user change the address.
    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.task = None;
        self.signin = None;
        self.server.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn render_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let has_server = !self.server.read(cx).value().trim().is_empty();
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(t("manage.romm.intro")),
            )
            .child(Input::new(&self.server).disabled(self.starting))
            .child(
                h_flex().justify_end().child(
                    Button::new("romm-continue")
                        .debug_selector(|| "romm-continue".into())
                        .primary()
                        .icon(IconName::ExternalLink)
                        .label(t("manage.romm.continue"))
                        .loading(self.starting)
                        .disabled(self.starting || !has_server)
                        .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
                ),
            )
            .into_any_element()
    }

    fn render_waiting(&self, signin: &SignIn, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let url = signin.url().to_string();
        v_flex()
            .gap_3()
            .child(div().text_sm().child(t("manage.romm.approve")))
            .child(
                div()
                    .text_2xl()
                    .font_weight(gpui_kit::FontWeight::BOLD)
                    .text_center()
                    .child(signin.user_code().to_string()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .text_color(muted)
                    .child(Spinner::new().small())
                    .child(div().flex_1().min_w_0().child(t("manage.romm.waiting")))
                    .child(
                        Button::new("romm-back")
                            .ghost()
                            .label(t("manage.romm.back"))
                            .on_click(cx.listener(|this, _, window, cx| this.back(window, cx))),
                    )
                    .child(
                        Button::new("romm-reopen")
                            .debug_selector(|| "romm-reopen".into())
                            .outline()
                            .icon(IconName::ExternalLink)
                            .label(t("manage.romm.reopen"))
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
            )
            .into_any_element()
    }
}

fn error_text(e: &anyhow::Error) -> SharedString {
    match e.downcast_ref::<DeviceError>() {
        Some(DeviceError::Denied) => t("manage.romm.error.denied"),
        Some(DeviceError::Expired) => t("manage.romm.error.expired"),
        Some(DeviceError::Unsupported) => t("manage.romm.error.unsupported"),
        None => format!("{e:#}").into(),
    }
}

impl Render for RommSignInView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.signin.clone() {
            Some(signin) => self.render_waiting(&signin, cx),
            None => self.render_form(cx),
        };
        v_flex()
            .gap_3()
            .child(body)
            .when_some(self.error.clone(), |this, e| {
                this.child(Alert::error("romm-sign-in-error", e))
            })
    }
}
