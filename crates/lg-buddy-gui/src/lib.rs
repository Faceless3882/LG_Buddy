mod brightness;

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use lg_buddy::brightness::{
    BrightnessApplication, BrightnessReadError, BrightnessReadOperation, BrightnessReader,
    BrightnessTransition, EnvironmentBrightnessReader,
};
use lg_buddy::presentation::brightness::{BrightnessFrontendUpdate, BrightnessIntent};

pub const APPLICATION_ID: &str = "io.github.staphylococcus.LGBuddy";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiCommand {
    Brightness,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuiParseError {
    MissingCommand,
    UnknownCommand(String),
    UnexpectedArguments(Vec<String>),
}

impl fmt::Display for GuiParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCommand => write!(f, "missing command; expected `brightness`"),
            Self::UnknownCommand(command) => write!(f, "unknown command `{command}`"),
            Self::UnexpectedArguments(arguments) => {
                write!(f, "unexpected arguments: {}", arguments.join(" "))
            }
        }
    }
}

pub fn parse_args<I, S>(args: I) -> Result<GuiCommand, GuiParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let command = match args.next() {
        Some(command) if command.as_ref() == "brightness" => GuiCommand::Brightness,
        Some(command) => return Err(GuiParseError::UnknownCommand(command.as_ref().to_string())),
        None => return Err(GuiParseError::MissingCommand),
    };

    let unexpected: Vec<String> = args.map(|argument| argument.as_ref().to_string()).collect();
    if !unexpected.is_empty() {
        return Err(GuiParseError::UnexpectedArguments(unexpected));
    }

    Ok(command)
}

pub fn help(program: &str) -> String {
    format!("Usage: {program} brightness\n")
}

pub fn run(command: GuiCommand) -> glib::ExitCode {
    match command {
        GuiCommand::Brightness => run_brightness_application(),
    }
}

fn run_brightness_application() -> glib::ExitCode {
    let application = adw::Application::builder()
        .application_id(APPLICATION_ID)
        .build();
    install_application_actions(&application);
    connect_brightness_application(&application, Arc::new(EnvironmentBrightnessReader));
    application.run_with_args(&["lg-buddy-gui"])
}

fn install_application_actions(application: &adw::Application) {
    let quit = gtk::gio::SimpleAction::new("quit", None);
    quit.connect_activate({
        let application = application.clone();
        move |_, _| application.quit()
    });
    application.add_action(&quit);
    application.set_accels_for_action("app.quit", &["<Primary>q"]);
}

struct BrightnessController {
    application: RefCell<BrightnessApplication>,
    window: brightness::BrightnessWindow,
    reader: Arc<dyn BrightnessReader>,
}

impl BrightnessController {
    fn new(
        gtk_application: &adw::Application,
        reader: Arc<dyn BrightnessReader>,
    ) -> (Rc<Self>, BrightnessTransition) {
        let (application, opening) = BrightnessApplication::open();
        let controller = Rc::new_cyclic(|controller| {
            let on_intent: brightness::IntentHandler = Rc::new({
                let controller = controller.clone();
                move |intent| {
                    if let Some(controller) = controller.upgrade() {
                        BrightnessController::handle_intent(&controller, intent);
                    }
                }
            });
            Self {
                application: RefCell::new(application),
                window: brightness::BrightnessWindow::new(gtk_application, on_intent),
                reader,
            }
        });
        (controller, opening)
    }

    fn present(&self) {
        self.window.present();
    }

    fn handle_intent(controller: &Rc<Self>, intent: BrightnessIntent) {
        let transition = controller.application.borrow_mut().handle_intent(intent);
        if let Some(transition) = transition {
            Self::apply_transition(controller, transition);
        }
    }

    fn complete_read(
        controller: &Rc<Self>,
        operation: BrightnessReadOperation,
        result: Result<lg_buddy::tv::OledBrightness, BrightnessReadError>,
    ) {
        let transition = controller
            .application
            .borrow_mut()
            .complete_read(operation, result);
        if let Some(transition) = transition {
            Self::apply_transition(controller, transition);
        }
    }

    fn apply_transition(controller: &Rc<Self>, transition: BrightnessTransition) {
        if let Some(diagnostic) = transition.diagnostic() {
            eprintln!("LG Buddy GUI: {diagnostic}");
        }
        match transition.update() {
            BrightnessFrontendUpdate::Present(presentation) => {
                controller.window.render(presentation);
                controller.window.present();
            }
            BrightnessFrontendUpdate::Close => controller.window.close(),
        }

        if let Some(operation) = transition.read_operation() {
            Self::start_read(controller, operation);
        }
    }

    fn start_read(controller: &Rc<Self>, operation: BrightnessReadOperation) {
        let reader = Arc::clone(&controller.reader);
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let _ = sender.send(reader.read_current_brightness());
        });

        let controller = Rc::downgrade(controller);
        glib::timeout_add_local(Duration::from_millis(10), move || {
            match receiver.try_recv() {
                Ok(result) => {
                    if let Some(controller) = controller.upgrade() {
                        BrightnessController::complete_read(&controller, operation, result);
                    }
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if let Some(controller) = controller.upgrade() {
                        BrightnessController::complete_read(
                            &controller,
                            operation,
                            Err(BrightnessReadError::new(
                                lg_buddy::brightness::BrightnessReadFailure::Internal,
                                "brightness reader stopped without returning a result",
                            )),
                        );
                    }
                    glib::ControlFlow::Break
                }
            }
        });
    }

    fn shutdown(&self) {
        self.application.borrow_mut().shutdown();
    }
}

fn connect_brightness_application(
    application: &adw::Application,
    reader: Arc<dyn BrightnessReader>,
) {
    let controller = Rc::new(RefCell::new(None::<Rc<BrightnessController>>));
    application.connect_activate({
        let controller = Rc::clone(&controller);
        let reader = Arc::clone(&reader);
        move |application| {
            if let Some(controller) = controller.borrow().as_ref() {
                controller.present();
                return;
            }

            let (brightness, opening) = BrightnessController::new(application, Arc::clone(&reader));
            controller.replace(Some(Rc::clone(&brightness)));
            BrightnessController::apply_transition(&brightness, opening);
        }
    });
    application.connect_shutdown(move |_| {
        if let Some(controller) = controller.borrow().as_ref() {
            controller.shutdown();
        }
    });
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    use gtk::glib;
    use gtk::prelude::*;
    use lg_buddy::brightness::{BrightnessApplication, BrightnessReadError, BrightnessReader};
    use lg_buddy::presentation::brightness::BrightnessFrontendUpdate;
    use lg_buddy::tv::OledBrightness;

    use super::{connect_brightness_application, help, parse_args, GuiCommand, GuiParseError};

    struct BlockingReader {
        results: Mutex<mpsc::Receiver<Result<OledBrightness, BrightnessReadError>>>,
    }

    impl BrightnessReader for BlockingReader {
        fn read_current_brightness(&self) -> Result<OledBrightness, BrightnessReadError> {
            self.results
                .lock()
                .expect("reader lock")
                .recv()
                .expect("test read result")
        }
    }

    #[test]
    fn parses_the_brightness_command() {
        assert_eq!(parse_args(["brightness"]), Ok(GuiCommand::Brightness));
        assert_eq!(
            parse_args(std::iter::empty::<&str>()),
            Err(GuiParseError::MissingCommand)
        );
        assert_eq!(
            parse_args(["settings"]),
            Err(GuiParseError::UnknownCommand("settings".to_string()))
        );
        assert_eq!(
            parse_args(["brightness", "extra"]),
            Err(GuiParseError::UnexpectedArguments(
                vec!["extra".to_string()]
            ))
        );
        assert_eq!(help("lg-buddy-gui"), "Usage: lg-buddy-gui brightness\n");
    }

    #[test]
    fn display_backed_application_remains_responsive_and_presents_the_read_result() {
        let (result_sender, result_receiver) = mpsc::channel();
        let reader = Arc::new(BlockingReader {
            results: Mutex::new(result_receiver),
        });
        let application_id = format!(
            "io.github.staphylococcus.LGBuddy.Test{}",
            std::process::id()
        );
        let application = adw::Application::builder()
            .application_id(application_id)
            .build();
        connect_brightness_application(&application, reader);

        let activation_count = Rc::new(Cell::new(0));
        let first_window = Rc::new(RefCell::new(None));
        application.connect_activate({
            let activation_count = Rc::clone(&activation_count);
            let first_window = Rc::clone(&first_window);
            move |application| {
                let current_count = activation_count.get() + 1;
                activation_count.set(current_count);

                let windows = application.windows();
                assert_eq!(windows.len(), 1);
                let window = windows[0].clone();
                if current_count == 1 {
                    super::brightness::assert_loading_window(
                        &window,
                        &lg_buddy::presentation::brightness::BrightnessPresentation::loading(),
                    );
                    super::brightness::assert_renderer_contract(application);
                    first_window.replace(Some(window.clone()));
                    let sender = result_sender.clone();
                    glib::idle_add_local_once(move || {
                        sender
                            .send(Ok(OledBrightness::new(72).expect("valid brightness")))
                            .expect("send read result");
                    });
                    wait_for_ready_then_reactivate(application.clone(), window);
                } else {
                    assert_eq!(first_window.borrow().as_ref(), Some(&window));
                    let presentation = ready_presentation(72);
                    super::brightness::assert_ready_window(&window, &presentation);
                    window.close();
                }
            }
        });

        let exit_code = application.run_with_args(&["lg-buddy-gui-test"]);

        assert_eq!(exit_code, glib::ExitCode::SUCCESS);
        assert_eq!(activation_count.get(), 2);
        assert!(application.windows().is_empty());
    }

    fn wait_for_ready_then_reactivate(application: adw::Application, window: gtk::Window) {
        glib::timeout_add_local(Duration::from_millis(10), move || {
            let has_scale = window
                .child()
                .is_some_and(|content| contains_scale(&content));
            if has_scale {
                application.activate();
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    fn contains_scale(widget: &gtk::Widget) -> bool {
        if widget.is::<gtk::Scale>() {
            return true;
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if contains_scale(&current) {
                return true;
            }
            child = current.next_sibling();
        }
        false
    }

    fn ready_presentation(value: u8) -> lg_buddy::presentation::brightness::BrightnessPresentation {
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
}
