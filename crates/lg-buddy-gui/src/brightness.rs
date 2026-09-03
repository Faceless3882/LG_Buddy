use std::cell::Cell;
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
        }
    }

    pub(crate) fn render(&self, presentation: &BrightnessPresentation) {
        self.window.set_title(Some(presentation.title()));
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
        self.body
            .append(&build_content(presentation, &self.on_intent));
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

fn build_content(presentation: &BrightnessPresentation, on_intent: &IntentHandler) -> gtk::Box {
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

    match presentation.status() {
        BrightnessStatus::Loading { message } => {
            content.append(&build_loading(message));
        }
        BrightnessStatus::Ready { message } => {
            let control = presentation
                .control()
                .expect("ready brightness presentation must declare a control");
            content.append(&build_ready(message, control));
        }
        BrightnessStatus::Failed(error) => {
            content.append(&build_failure(error.summary(), error.detail()));
        }
    }

    content.append(&build_actions(presentation, on_intent));
    content
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

fn build_ready(
    message: &str,
    control: &lg_buddy::presentation::brightness::BrightnessControl,
) -> gtk::Box {
    let ready = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    let status = gtk::Label::builder()
        .label(message)
        .wrap(true)
        .xalign(0.0)
        .build();
    let scale = gtk::Scale::with_range(
        gtk::Orientation::Horizontal,
        f64::from(control.minimum()),
        f64::from(control.maximum()),
        f64::from(control.step()),
    );
    scale.set_value(f64::from(control.proposed().as_percent()));
    scale.set_digits(0);
    scale.set_draw_value(true);
    scale.set_hexpand(true);
    scale.set_sensitive(control.enabled());
    scale.update_property(&[
        gtk::accessible::Property::Label(control.label()),
        gtk::accessible::Property::Description(message),
    ]);

    ready.append(&status);
    ready.append(&scale);
    ready
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

fn build_actions(presentation: &BrightnessPresentation, on_intent: &IntentHandler) -> gtk::Box {
    let actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    if let Some(primary) = presentation.primary_action() {
        actions.append(&build_action_button(primary, on_intent));
    }
    actions.append(&build_action_button(
        presentation.cancel_action(),
        on_intent,
    ));
    actions
}

fn build_action_button(action: &ActionPresentation, on_intent: &IntentHandler) -> gtk::Button {
    let button = gtk::Button::with_label(action.label());
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
    let status = ready
        .first_child()
        .expect("brightness ready content should have a status label")
        .downcast::<gtk::Label>()
        .expect("brightness ready status should be a label");
    let BrightnessStatus::Ready { message } = presentation.status() else {
        panic!("expected ready presentation");
    };
    assert_eq!(status.label(), message.as_str());

    let scale = status
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
    let first = actions
        .first_child()
        .expect("brightness window should have a cancel action");
    let cancel = if let Some(primary) = presentation.primary_action() {
        assert_action_button(&first, primary);
        first
            .next_sibling()
            .expect("brightness window should have a cancel action")
    } else {
        first
    };
    assert_action_button(&cancel, presentation.cancel_action());
    assert!(cancel.next_sibling().is_none());
}

#[cfg(test)]
fn assert_action_button(widget: &gtk::Widget, action: &ActionPresentation) {
    let button = widget
        .clone()
        .downcast::<gtk::Button>()
        .expect("brightness action should be a button");
    assert_eq!(button.label().as_deref(), Some(action.label()));
    assert_eq!(button.is_sensitive(), action.enabled());
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
    use lg_buddy::brightness::{BrightnessApplication, BrightnessReadError, BrightnessReadFailure};
    use lg_buddy::presentation::brightness::{
        BrightnessFrontendUpdate, BrightnessIntent, BrightnessPresentation,
    };
    use lg_buddy::tv::OledBrightness;

    use super::{assert_failed_window, assert_ready_window, BrightnessWindow, IntentHandler};

    pub(super) fn assert_renderer_contract(application: &adw::Application) {
        assert_loading_presentation(application);
        assert_ready_presentation(application);
        assert_failed_presentation(application);
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
        let retry = actions
            .first_child()
            .expect("retry action")
            .downcast::<gtk::Button>()
            .expect("retry button");
        let cancel = retry
            .next_sibling()
            .expect("cancel action")
            .downcast::<gtk::Button>()
            .expect("cancel button");

        retry.emit_clicked();
        cancel.emit_clicked();

        assert_eq!(
            intents.borrow().as_slice(),
            &[BrightnessIntent::Retry, BrightnessIntent::Cancel]
        );
        view.close();
    }

    fn test_window(application: &adw::Application, on_intent: IntentHandler) -> BrightnessWindow {
        BrightnessWindow::new(application, on_intent)
    }

    fn ready_presentation(value: u8) -> BrightnessPresentation {
        let (mut application, opening) = BrightnessApplication::open();
        let operation = opening.read_operation().expect("opening read");
        let transition = application
            .complete_read(
                operation,
                Ok(OledBrightness::new(value).expect("valid brightness")),
            )
            .expect("ready transition");
        let BrightnessFrontendUpdate::Present(presentation) = transition.update() else {
            panic!("ready transition should present");
        };
        presentation.clone()
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
        let BrightnessFrontendUpdate::Present(presentation) = transition.update() else {
            panic!("failed transition should present");
        };
        presentation.clone()
    }
}
