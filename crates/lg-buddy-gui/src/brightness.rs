use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[cfg(test)]
use adw::prelude::AdwApplicationWindowExt;
use gtk::prelude::*;
use lg_buddy::presentation::brightness::{
    ActionPresentation, BrightnessIntent, BrightnessPresentation, BrightnessStatus,
};

pub(crate) type IntentHandler = Rc<dyn Fn(BrightnessIntent)>;

pub(crate) struct BrightnessWindow {
    window: adw::ApplicationWindow,
    body: gtk::Box,
    allow_close: Rc<Cell<bool>>,
    close_requested: Rc<Cell<bool>>,
    on_intent: IntentHandler,
    interactive: RefCell<Option<InteractiveView>>,
}

struct InteractiveView {
    content: gtk::Box,
    status: gtk::Box,
    scale: gtk::Scale,
    actions: gtk::Box,
    suppress_proposal: Rc<Cell<bool>>,
}

impl BrightnessWindow {
    pub(crate) fn new(application: &adw::Application, on_intent: IntentHandler) -> Self {
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.set_vexpand(true);
        let shell = gtk::Box::new(gtk::Orientation::Vertical, 0);
        shell.append(&adw::HeaderBar::new());
        shell.append(&body);
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .icon_name(crate::APPLICATION_ID)
            .default_width(360)
            .content(&shell)
            .build();
        let allow_close = Rc::new(Cell::new(false));
        let close_requested = Rc::new(Cell::new(false));
        window.connect_close_request({
            let allow_close = Rc::clone(&allow_close);
            let close_requested = Rc::clone(&close_requested);
            let on_intent = Rc::clone(&on_intent);
            move |_| {
                if allow_close.get() {
                    gtk::glib::Propagation::Proceed
                } else {
                    close_requested.set(true);
                    on_intent(BrightnessIntent::Cancel);
                    close_requested.set(false);
                    if allow_close.get() {
                        gtk::glib::Propagation::Proceed
                    } else {
                        gtk::glib::Propagation::Stop
                    }
                }
            }
        });

        Self {
            window,
            body,
            allow_close,
            close_requested,
            on_intent,
            interactive: RefCell::new(None),
        }
    }

    pub(crate) fn render(&self, presentation: &BrightnessPresentation) {
        self.window.set_title(Some(presentation.title()));
        if presentation.control().is_some() {
            let mut interactive = self.interactive.borrow_mut();
            if interactive.is_none() {
                let view = InteractiveView::new(presentation, &self.on_intent);
                replace_child(&self.body, &view.content);
                *interactive = Some(view);
            }
            let primary = interactive
                .as_ref()
                .expect("interactive brightness presentation must have a view")
                .render(presentation, &self.on_intent);
            self.window
                .set_default_widget(primary.as_ref().filter(|button| button.is_sensitive()));
        } else {
            self.interactive.borrow_mut().take();
            let (content, primary) = build_content(presentation, &self.on_intent);
            replace_child(&self.body, &content);
            self.window
                .set_default_widget(primary.as_ref().filter(|button| button.is_sensitive()));
        }
    }

    pub(crate) fn present(&self) {
        self.window.present();
    }

    pub(crate) fn close(&self) {
        self.allow_close.set(true);
        if !self.close_requested.get() {
            self.window.close();
        }
    }

    #[cfg(test)]
    pub(crate) fn window(&self) -> gtk::Window {
        self.window.clone().upcast()
    }
}

impl InteractiveView {
    fn new(presentation: &BrightnessPresentation, on_intent: &IntentHandler) -> Self {
        let content = content_shell(presentation);
        let status = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scale = gtk::Scale::new(gtk::Orientation::Horizontal, None::<&gtk::Adjustment>);
        scale.set_digits(0);
        scale.set_draw_value(true);
        scale.set_hexpand(true);
        scale.set_focusable(true);
        let suppress_proposal = Rc::new(Cell::new(false));
        scale.connect_value_changed({
            let suppress_proposal = Rc::clone(&suppress_proposal);
            let on_intent = Rc::clone(on_intent);
            move |scale| {
                if suppress_proposal.get() {
                    return;
                }
                let value = scale.value();
                assert!(
                    value.is_finite()
                        && value.fract() == 0.0
                        && value >= f64::from(u8::MIN)
                        && value <= f64::from(u8::MAX),
                    "GTK returned a non-integral brightness outside the renderer range"
                );
                on_intent(BrightnessIntent::Propose(value as u8));
            }
        });
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        body.append(&status);
        body.append(&scale);
        content.append(&body);
        let actions = action_box();
        content.append(&actions);

        Self {
            content,
            status,
            scale,
            actions,
            suppress_proposal,
        }
    }

    fn render(
        &self,
        presentation: &BrightnessPresentation,
        on_intent: &IntentHandler,
    ) -> Option<gtk::Button> {
        let control = presentation
            .control()
            .expect("interactive brightness presentation must declare a control");
        replace_status(&self.status, presentation.status());

        self.suppress_proposal.set(true);
        self.scale.adjustment().configure(
            f64::from(control.proposed().as_percent()),
            f64::from(control.minimum()),
            f64::from(control.maximum()),
            f64::from(control.step()),
            f64::from(control.step().saturating_mul(5)),
            0.0,
        );
        self.scale.set_sensitive(control.enabled());
        self.scale.update_property(&[
            gtk::accessible::Property::Label(control.label()),
            gtk::accessible::Property::Description(&control_description(presentation.status())),
        ]);
        self.suppress_proposal.set(false);

        rebuild_actions(&self.actions, presentation, on_intent)
    }
}

fn replace_child(container: &gtk::Box, child: &impl IsA<gtk::Widget>) {
    while let Some(existing) = container.first_child() {
        container.remove(&existing);
    }
    container.append(child);
}

fn content_shell(presentation: &BrightnessPresentation) -> gtk::Box {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();

    let heading = gtk::Label::builder()
        .label(presentation.heading())
        .xalign(0.0)
        .accessible_role(gtk::AccessibleRole::Heading)
        .build();
    heading.add_css_class("title-2");
    content.append(&heading);

    content
}

fn build_content(
    presentation: &BrightnessPresentation,
    on_intent: &IntentHandler,
) -> (gtk::Box, Option<gtk::Button>) {
    let content = content_shell(presentation);

    match presentation.status() {
        BrightnessStatus::Loading { message } => {
            content.append(&build_loading(message));
        }
        BrightnessStatus::Ready { message } => {
            panic!("ready presentation must use the interactive renderer: {message}");
        }
        BrightnessStatus::Applying { message } => {
            panic!("applying presentation must use the interactive renderer: {message}");
        }
        BrightnessStatus::Failed(error) => {
            content.append(&build_failure(error.summary(), error.detail()));
        }
    }

    let actions = action_box();
    let primary = rebuild_actions(&actions, presentation, on_intent);
    content.append(&actions);
    (content, primary)
}

fn build_loading(message: &str) -> gtk::Box {
    let loading = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    let spinner = gtk::Spinner::builder()
        .spinning(true)
        .accessible_role(gtk::AccessibleRole::Status)
        .build();
    spinner.update_property(&[
        gtk::accessible::Property::Label(message),
        gtk::accessible::Property::Description(
            "LG Buddy is waiting for the current TV brightness.",
        ),
    ]);
    let status = gtk::Label::builder()
        .label(message)
        .wrap(true)
        .xalign(0.0)
        .build();

    loading.append(&spinner);
    loading.append(&status);
    loading
}

fn replace_status(container: &gtk::Box, status: &BrightnessStatus) {
    let child: gtk::Widget = match status {
        BrightnessStatus::Ready { message } => gtk::Label::builder()
            .label(message)
            .wrap(true)
            .xalign(0.0)
            .build()
            .upcast(),
        BrightnessStatus::Applying { message } => {
            let applying = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(12)
                .build();
            let spinner = gtk::Spinner::builder()
                .spinning(true)
                .accessible_role(gtk::AccessibleRole::Status)
                .build();
            spinner.update_property(&[
                gtk::accessible::Property::Label(message),
                gtk::accessible::Property::Description(
                    "LG Buddy is waiting for the TV to apply the brightness.",
                ),
            ]);
            applying.append(&spinner);
            applying.append(
                &gtk::Label::builder()
                    .label(message)
                    .wrap(true)
                    .xalign(0.0)
                    .build(),
            );
            applying.upcast()
        }
        BrightnessStatus::Failed(error) => build_failure(error.summary(), error.detail()).upcast(),
        BrightnessStatus::Loading { .. } => {
            panic!("loading presentation cannot declare an interactive control")
        }
    };
    replace_child(container, &child);
}

fn control_description(status: &BrightnessStatus) -> String {
    match status {
        BrightnessStatus::Ready { message } | BrightnessStatus::Applying { message } => {
            message.clone()
        }
        BrightnessStatus::Failed(error) => format!("{} {}", error.summary(), error.detail()),
        BrightnessStatus::Loading { .. } => {
            panic!("loading presentation cannot declare an interactive control")
        }
    }
}

fn build_failure(summary: &str, detail: &str) -> gtk::Box {
    let failure = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    let summary = gtk::Label::builder()
        .label(summary)
        .wrap(true)
        .xalign(0.0)
        .accessible_role(gtk::AccessibleRole::Alert)
        .build();
    let detail = gtk::Label::builder()
        .label(detail)
        .wrap(true)
        .xalign(0.0)
        .build();

    failure.append(&summary);
    failure.append(&detail);
    failure
}

fn action_box() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build()
}

fn rebuild_actions(
    actions: &gtk::Box,
    presentation: &BrightnessPresentation,
    on_intent: &IntentHandler,
) -> Option<gtk::Button> {
    while let Some(child) = actions.first_child() {
        actions.remove(&child);
    }
    actions.append(&build_action_button(
        presentation.cancel_action(),
        on_intent,
    ));
    let primary = presentation.primary_action().map(|action| {
        let button = build_action_button(action, on_intent);
        button.add_css_class("suggested-action");
        actions.append(&button);
        button
    });
    primary
}

fn build_action_button(action: &ActionPresentation, on_intent: &IntentHandler) -> gtk::Button {
    let button = gtk::Button::with_mnemonic(&format!("_{}", action.label()));
    button.set_sensitive(action.enabled());
    button.connect_clicked({
        let on_intent = Rc::clone(on_intent);
        let intent = action.intent();
        move |_| on_intent(intent)
    });
    button
}

#[cfg(test)]
pub(crate) fn assert_loading_window(window: &gtk::Window, presentation: &BrightnessPresentation) {
    assert_common_window(window, presentation);
    let content = window_content(window);
    let loading = content_body(&content)
        .downcast::<gtk::Box>()
        .expect("brightness loading content should be a box");
    let spinner = loading
        .first_child()
        .expect("brightness loading content should have a spinner")
        .downcast::<gtk::Spinner>()
        .expect("brightness loading indicator should be a spinner");
    assert!(spinner.is_spinning());
    assert_eq!(spinner.accessible_role(), gtk::AccessibleRole::Status);

    let status = spinner
        .next_sibling()
        .expect("brightness loading content should have a status label")
        .downcast::<gtk::Label>()
        .expect("brightness loading status should be a label");
    let BrightnessStatus::Loading { message } = presentation.status() else {
        panic!("expected loading presentation");
    };
    assert_eq!(status.label(), message.as_str());
}

#[cfg(test)]
pub(crate) fn assert_ready_window(window: &gtk::Window, presentation: &BrightnessPresentation) {
    assert_common_window(window, presentation);
    let content = window_content(window);
    let ready = content_body(&content)
        .downcast::<gtk::Box>()
        .expect("brightness ready content should be a box");
    let status_container = ready
        .first_child()
        .expect("brightness ready content should have a status container")
        .downcast::<gtk::Box>()
        .expect("brightness ready status should be a box");
    let status = status_container
        .first_child()
        .expect("brightness ready status should have a label")
        .downcast::<gtk::Label>()
        .expect("brightness ready status should be a label");
    let BrightnessStatus::Ready { message } = presentation.status() else {
        panic!("expected ready presentation");
    };
    assert_eq!(status.label(), message.as_str());

    let scale = status_container
        .next_sibling()
        .expect("brightness ready content should have a scale")
        .downcast::<gtk::Scale>()
        .expect("brightness control should be a scale");
    let control = presentation.control().expect("ready control");
    assert_eq!(scale.value(), f64::from(control.proposed().as_percent()));
    assert_eq!(scale.adjustment().lower(), f64::from(control.minimum()));
    assert_eq!(scale.adjustment().upper(), f64::from(control.maximum()));
    assert_eq!(
        scale.adjustment().step_increment(),
        f64::from(control.step())
    );
    assert_eq!(scale.is_sensitive(), control.enabled());
    assert_eq!(scale.accessible_role(), gtk::AccessibleRole::Slider);
    assert!(scale.is_focusable());
}

#[cfg(test)]
pub(crate) fn assert_applying_window(window: &gtk::Window, presentation: &BrightnessPresentation) {
    assert_common_window(window, presentation);
    let content = window_content(window);
    let applying = content_body(&content)
        .downcast::<gtk::Box>()
        .expect("brightness applying content should be a box");
    let status_container = applying
        .first_child()
        .expect("brightness applying content should have a status container")
        .downcast::<gtk::Box>()
        .expect("brightness applying status should be a box");
    let status = status_container
        .first_child()
        .expect("brightness applying status should have content")
        .downcast::<gtk::Box>()
        .expect("brightness applying status should be a box");
    let spinner = status
        .first_child()
        .expect("brightness applying status should have a spinner")
        .downcast::<gtk::Spinner>()
        .expect("brightness applying indicator should be a spinner");
    assert!(spinner.is_spinning());
    assert_eq!(spinner.accessible_role(), gtk::AccessibleRole::Status);

    let scale = status_container
        .next_sibling()
        .expect("brightness applying content should have a scale")
        .downcast::<gtk::Scale>()
        .expect("brightness applying control should be a scale");
    let control = presentation.control().expect("applying control");
    assert_eq!(scale.value(), f64::from(control.proposed().as_percent()));
    assert!(!scale.is_sensitive());
}

#[cfg(test)]
pub(crate) fn assert_failed_window(window: &gtk::Window, presentation: &BrightnessPresentation) {
    assert_common_window(window, presentation);
    let content = window_content(window);
    let failure = content_body(&content)
        .downcast::<gtk::Box>()
        .expect("brightness failure content should be a box");
    let summary = failure
        .first_child()
        .expect("brightness failure should have a summary")
        .downcast::<gtk::Label>()
        .expect("brightness failure summary should be a label");
    let detail = summary
        .next_sibling()
        .expect("brightness failure should have detail")
        .downcast::<gtk::Label>()
        .expect("brightness failure detail should be a label");
    let BrightnessStatus::Failed(error) = presentation.status() else {
        panic!("expected failed presentation");
    };
    assert_eq!(summary.label(), error.summary());
    assert_eq!(summary.accessible_role(), gtk::AccessibleRole::Alert);
    assert_eq!(detail.label(), error.detail());
}

#[cfg(test)]
pub(crate) fn assert_write_failed_window(
    window: &gtk::Window,
    presentation: &BrightnessPresentation,
) {
    assert_common_window(window, presentation);
    let content = window_content(window);
    let failed = content_body(&content)
        .downcast::<gtk::Box>()
        .expect("brightness write failure content should be a box");
    let status_container = failed
        .first_child()
        .expect("brightness write failure should have a status container")
        .downcast::<gtk::Box>()
        .expect("brightness write failure status should be a box");
    let failure = status_container
        .first_child()
        .expect("brightness write failure should have error content")
        .downcast::<gtk::Box>()
        .expect("brightness write failure should be a box");
    let summary = failure
        .first_child()
        .expect("brightness write failure should have a summary")
        .downcast::<gtk::Label>()
        .expect("brightness write failure summary should be a label");
    assert_eq!(summary.accessible_role(), gtk::AccessibleRole::Alert);

    let scale = status_container
        .next_sibling()
        .expect("brightness write failure should preserve the scale")
        .downcast::<gtk::Scale>()
        .expect("brightness write failure control should be a scale");
    let control = presentation.control().expect("failed write control");
    assert_eq!(scale.value(), f64::from(control.proposed().as_percent()));
    assert!(scale.is_sensitive());
}

#[cfg(test)]
fn assert_common_window(window: &gtk::Window, presentation: &BrightnessPresentation) {
    assert!(window.is_visible());
    assert_eq!(window.title().as_deref(), Some(presentation.title()));
    assert_eq!(window.icon_name().as_deref(), Some(crate::APPLICATION_ID));
    let content = window_content(window);
    let heading = content
        .first_child()
        .expect("brightness window should have a heading")
        .downcast::<gtk::Label>()
        .expect("brightness heading should be a label");
    assert_eq!(heading.label(), presentation.heading());
    assert_eq!(heading.accessible_role(), gtk::AccessibleRole::Heading);
    assert_actions(&content, presentation);
}

#[cfg(test)]
fn assert_actions(content: &gtk::Box, presentation: &BrightnessPresentation) {
    let actions = content
        .last_child()
        .expect("brightness window should have actions")
        .downcast::<gtk::Box>()
        .expect("brightness actions should be a box");
    let cancel = actions
        .first_child()
        .expect("brightness window should have a cancel action");
    assert_action_button(&cancel, presentation.cancel_action());
    if let Some(primary) = presentation.primary_action() {
        let primary_widget = cancel
            .next_sibling()
            .expect("brightness window should have a primary action");
        assert_action_button(&primary_widget, primary);
        assert!(primary_widget.next_sibling().is_none());
    } else {
        assert!(cancel.next_sibling().is_none());
    }
}

#[cfg(test)]
fn assert_action_button(widget: &gtk::Widget, action: &ActionPresentation) {
    let button = widget
        .clone()
        .downcast::<gtk::Button>()
        .expect("brightness action should be a button");
    let mnemonic_label = format!("_{}", action.label());
    assert_eq!(button.label().as_deref(), Some(mnemonic_label.as_str()));
    assert_eq!(button.is_sensitive(), action.enabled());
    assert!(button.is_focusable());
}

#[cfg(test)]
fn window_content(window: &gtk::Window) -> gtk::Box {
    window
        .clone()
        .downcast::<adw::ApplicationWindow>()
        .expect("brightness window should use the Adwaita application shell")
        .content()
        .expect("brightness window should have an Adwaita content shell")
        .downcast::<gtk::Box>()
        .expect("brightness window shell should be a box")
        .last_child()
        .expect("brightness window shell should have a body")
        .downcast::<gtk::Box>()
        .expect("brightness window body should be a box")
        .first_child()
        .expect("brightness window body should have presentation content")
        .downcast::<gtk::Box>()
        .expect("brightness presentation content should be a box")
}

#[cfg(test)]
fn content_body(content: &gtk::Box) -> gtk::Widget {
    content
        .first_child()
        .expect("brightness window should have a heading")
        .next_sibling()
        .expect("brightness window should have body content")
}

#[cfg(test)]
pub(crate) fn assert_renderer_contract(application: &adw::Application) {
    tests::assert_renderer_contract(application);
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gtk::prelude::*;
    use lg_buddy::brightness::{
        BrightnessApplication, BrightnessReadError, BrightnessReadFailure, BrightnessWriteError,
        BrightnessWriteFailure,
    };
    use lg_buddy::presentation::brightness::{
        BrightnessFrontendUpdate, BrightnessIntent, BrightnessPresentation,
    };
    use lg_buddy::tv::OledBrightness;

    use super::{
        assert_applying_window, assert_failed_window, assert_ready_window,
        assert_write_failed_window, BrightnessWindow, IntentHandler,
    };

    pub(super) fn assert_renderer_contract(application: &adw::Application) {
        assert_loading_presentation(application);
        assert_ready_presentation(application);
        assert_applying_presentation(application);
        assert_failed_presentation(application);
        assert_write_failed_presentation(application);
    }

    fn assert_loading_presentation(application: &adw::Application) {
        let intents = Rc::new(RefCell::new(Vec::new()));
        let view = test_window(
            application,
            Rc::new({
                let intents = Rc::clone(&intents);
                move |intent| intents.borrow_mut().push(intent)
            }),
        );
        let presentation = BrightnessPresentation::loading();

        view.render(&presentation);
        view.present();

        super::assert_loading_window(&view.window(), &presentation);
        assert!(intents.borrow().is_empty());
        view.close();
    }

    fn assert_ready_presentation(application: &adw::Application) {
        let intents = Rc::new(RefCell::new(Vec::new()));
        let view = test_window(
            application,
            Rc::new({
                let intents = Rc::clone(&intents);
                move |intent| intents.borrow_mut().push(intent)
            }),
        );
        let presentation = ready_presentation(72);

        view.render(&presentation);
        view.present();

        assert_ready_window(&view.window(), &presentation);
        assert!(intents.borrow().is_empty());

        let original_scale = interactive_scale(&view);
        assert!(original_scale.grab_focus());
        original_scale.set_value(65.0);
        assert_eq!(
            intents.borrow().as_slice(),
            &[BrightnessIntent::Propose(65)]
        );

        let proposed = proposed_presentation(72, 65);
        view.render(&proposed);
        assert_ready_window(&view.window(), &proposed);
        assert_eq!(original_scale, interactive_scale(&view));
        assert_eq!(
            intents.borrow().as_slice(),
            &[BrightnessIntent::Propose(65)],
            "applying a presentation must not echo a proposal"
        );

        let apply = primary_action(&view);
        assert!(apply.is_sensitive());
        assert_eq!(view.window().default_widget(), Some(apply.clone().upcast()));
        apply.emit_clicked();
        assert_eq!(
            intents.borrow().as_slice(),
            &[BrightnessIntent::Propose(65), BrightnessIntent::Apply]
        );
        view.close();
    }

    fn assert_applying_presentation(application: &adw::Application) {
        let intents = Rc::new(RefCell::new(Vec::new()));
        let view = test_window(
            application,
            Rc::new({
                let intents = Rc::clone(&intents);
                move |intent| intents.borrow_mut().push(intent)
            }),
        );
        let presentation = applying_presentation(72, 65);

        view.render(&presentation);
        view.present();

        assert_applying_window(&view.window(), &presentation);
        assert!(intents.borrow().is_empty());
        assert!(!primary_action(&view).is_sensitive());
        assert!(view.window().default_widget().is_none());
        view.close();
    }

    fn assert_failed_presentation(application: &adw::Application) {
        let intents = Rc::new(RefCell::new(Vec::new()));
        let view = test_window(
            application,
            Rc::new({
                let intents = Rc::clone(&intents);
                move |intent| intents.borrow_mut().push(intent)
            }),
        );
        let presentation = failed_presentation();

        view.render(&presentation);
        view.present();
        assert_failed_window(&view.window(), &presentation);
        assert!(intents.borrow().is_empty());

        let content = super::window_content(&view.window());
        let actions = content
            .last_child()
            .expect("actions")
            .downcast::<gtk::Box>()
            .expect("actions box");
        let cancel = actions
            .first_child()
            .expect("cancel action")
            .downcast::<gtk::Button>()
            .expect("cancel button");
        let retry = cancel
            .next_sibling()
            .expect("retry action")
            .downcast::<gtk::Button>()
            .expect("retry button");

        retry.emit_clicked();
        cancel.emit_clicked();

        assert_eq!(
            intents.borrow().as_slice(),
            &[BrightnessIntent::Retry, BrightnessIntent::Cancel]
        );
        view.close();
    }

    fn assert_write_failed_presentation(application: &adw::Application) {
        let intents = Rc::new(RefCell::new(Vec::new()));
        let view = test_window(
            application,
            Rc::new({
                let intents = Rc::clone(&intents);
                move |intent| intents.borrow_mut().push(intent)
            }),
        );
        let presentation = write_failed_presentation(72, 65);

        view.render(&presentation);
        view.present();
        assert_write_failed_window(&view.window(), &presentation);
        assert!(intents.borrow().is_empty());

        interactive_scale(&view).set_value(60.0);
        primary_action(&view).emit_clicked();
        cancel_action(&view).emit_clicked();

        assert_eq!(
            intents.borrow().as_slice(),
            &[
                BrightnessIntent::Propose(60),
                BrightnessIntent::Retry,
                BrightnessIntent::Cancel,
            ]
        );
        view.close();
    }

    fn test_window(application: &adw::Application, on_intent: IntentHandler) -> BrightnessWindow {
        BrightnessWindow::new(application, on_intent)
    }

    fn ready_presentation(value: u8) -> BrightnessPresentation {
        presentation_after_read(value).1
    }

    fn proposed_presentation(current: u8, proposed: u8) -> BrightnessPresentation {
        let (mut application, _) = presentation_after_read(current);
        presented(
            application
                .handle_intent(BrightnessIntent::Propose(proposed))
                .expect("proposal transition"),
        )
    }

    fn applying_presentation(current: u8, proposed: u8) -> BrightnessPresentation {
        let (mut application, _) = presentation_after_read(current);
        application
            .handle_intent(BrightnessIntent::Propose(proposed))
            .expect("proposal transition");
        presented(
            application
                .handle_intent(BrightnessIntent::Apply)
                .expect("apply transition"),
        )
    }

    fn write_failed_presentation(current: u8, proposed: u8) -> BrightnessPresentation {
        let (mut application, _) = presentation_after_read(current);
        application
            .handle_intent(BrightnessIntent::Propose(proposed))
            .expect("proposal transition");
        let applying = application
            .handle_intent(BrightnessIntent::Apply)
            .expect("apply transition");
        let operation = applying.write_operation().expect("write operation");
        presented(
            application
                .complete_write(
                    operation,
                    Err(BrightnessWriteError::new(
                        BrightnessWriteFailure::Unreachable,
                        "test diagnostic",
                    )),
                )
                .expect("failed write transition"),
        )
    }

    fn presentation_after_read(value: u8) -> (BrightnessApplication, BrightnessPresentation) {
        let (mut application, opening) = BrightnessApplication::open();
        let operation = opening.read_operation().expect("opening read");
        let transition = application
            .complete_read(
                operation,
                Ok(OledBrightness::new(value).expect("valid brightness")),
            )
            .expect("ready transition");
        let presentation = presented(transition);
        (application, presentation)
    }

    fn failed_presentation() -> BrightnessPresentation {
        let (mut application, opening) = BrightnessApplication::open();
        let operation = opening.read_operation().expect("opening read");
        let transition = application
            .complete_read(
                operation,
                Err(BrightnessReadError::new(
                    BrightnessReadFailure::Unreachable,
                    "test diagnostic",
                )),
            )
            .expect("failed transition");
        presented(transition)
    }

    fn presented(transition: lg_buddy::brightness::BrightnessTransition) -> BrightnessPresentation {
        let BrightnessFrontendUpdate::Present(presentation) = transition.update() else {
            panic!("transition should present");
        };
        presentation.clone()
    }

    fn interactive_scale(view: &BrightnessWindow) -> gtk::Scale {
        let body = super::content_body(&super::window_content(&view.window()))
            .downcast::<gtk::Box>()
            .expect("interactive body");
        body.last_child()
            .expect("interactive scale")
            .downcast::<gtk::Scale>()
            .expect("scale")
    }

    fn primary_action(view: &BrightnessWindow) -> gtk::Button {
        action_box(view)
            .last_child()
            .expect("primary action")
            .downcast::<gtk::Button>()
            .expect("primary button")
    }

    fn cancel_action(view: &BrightnessWindow) -> gtk::Button {
        action_box(view)
            .first_child()
            .expect("cancel action")
            .downcast::<gtk::Button>()
            .expect("cancel button")
    }

    fn action_box(view: &BrightnessWindow) -> gtk::Box {
        super::window_content(&view.window())
            .last_child()
            .expect("actions")
            .downcast::<gtk::Box>()
            .expect("actions box")
    }
}
