mod brightness;

use std::fmt;

use gtk::glib;
use gtk::prelude::*;
use lg_buddy::presentation::brightness::BrightnessPresentation;

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
    let application = gtk::Application::builder()
        .application_id(APPLICATION_ID)
        .build();
    install_application_actions(&application);
    connect_brightness_application(&application, BrightnessPresentation::loading());
    application.run_with_args(&["lg-buddy-gui"])
}

fn install_application_actions(application: &gtk::Application) {
    let quit = gtk::gio::SimpleAction::new("quit", None);
    quit.connect_activate({
        let application = application.clone();
        move |_, _| application.quit()
    });
    application.add_action(&quit);
    application.set_accels_for_action("app.quit", &["<Primary>q"]);
}

fn connect_brightness_application(
    application: &gtk::Application,
    presentation: BrightnessPresentation,
) {
    application.connect_activate(move |application| {
        brightness::present(application, &presentation);
    });
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use gtk::glib;
    use gtk::prelude::*;
    use lg_buddy::presentation::brightness::BrightnessPresentation;

    use super::{connect_brightness_application, help, parse_args, GuiCommand, GuiParseError};

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
    fn display_backed_application_presents_one_loading_window_and_closes() {
        let presentation = BrightnessPresentation::loading();
        let application_id = format!(
            "io.github.staphylococcus.LGBuddy.Test{}",
            std::process::id()
        );
        let application = gtk::Application::builder()
            .application_id(application_id)
            .build();
        connect_brightness_application(&application, presentation.clone());

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
                super::brightness::assert_loading_window(&window, &presentation);

                if current_count == 1 {
                    first_window.replace(Some(window));
                    let application = application.clone();
                    glib::idle_add_local_once(move || application.activate());
                } else {
                    assert_eq!(first_window.borrow().as_ref(), Some(&window));
                    window.close();
                    application.quit();
                }
            }
        });

        let exit_code = application.run_with_args(&["lg-buddy-gui-test"]);

        assert_eq!(exit_code, glib::ExitCode::SUCCESS);
        assert_eq!(activation_count.get(), 2);
        assert!(application.windows().is_empty());
    }
}
