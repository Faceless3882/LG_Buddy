use gtk::prelude::*;
use lg_buddy::presentation::brightness::{BrightnessPresentation, BrightnessStatus};

pub(crate) fn present(
    application: &gtk::Application,
    presentation: &BrightnessPresentation,
) -> gtk::Window {
    if let Some(window) = application.windows().into_iter().next() {
        window.present();
        return window;
    }

    let window = build_window(application, presentation);
    window.present();
    window.upcast()
}

fn build_window(
    application: &gtk::Application,
    presentation: &BrightnessPresentation,
) -> gtk::ApplicationWindow {
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
            content.append(&loading);
        }
    }

    gtk::ApplicationWindow::builder()
        .application(application)
        .title(presentation.title())
        .default_width(360)
        .child(&content)
        .build()
}

#[cfg(test)]
pub(crate) fn assert_loading_window(window: &gtk::Window, presentation: &BrightnessPresentation) {
    assert!(window.is_visible());
    assert_eq!(window.title().as_deref(), Some(presentation.title()));

    let content = window
        .child()
        .expect("brightness window should have content")
        .downcast::<gtk::Box>()
        .expect("brightness window content should be a box");
    let heading = content
        .first_child()
        .expect("brightness window should have a heading")
        .downcast::<gtk::Label>()
        .expect("brightness heading should be a label");
    assert_eq!(heading.label(), presentation.heading());
    assert_eq!(heading.accessible_role(), gtk::AccessibleRole::Heading);

    let loading = heading
        .next_sibling()
        .expect("brightness window should have loading content")
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
    let BrightnessStatus::Loading { message } = presentation.status();
    assert_eq!(status.label(), message.as_str());
    assert_eq!(status.accessible_role(), gtk::AccessibleRole::Label);
}
