use gtk::glib;
use lg_buddy_gui::{help, parse_args, run};

fn main() -> glib::ExitCode {
    let program = std::env::args()
        .next()
        .unwrap_or_else(|| "lg-buddy-gui".to_string());

    match parse_args(std::env::args().skip(1)) {
        Ok(command) => run(command),
        Err(err) => {
            eprintln!("LG Buddy GUI: {err}");
            eprintln!();
            eprint!("{}", help(&program));
            glib::ExitCode::FAILURE
        }
    }
}
