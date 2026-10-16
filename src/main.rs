fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!(
            "hyprselector — keyboard window hints for Hyprland\n\nUsage: hyprselector\n\nType a window label to focus it and center the pointer.\nEsc cancels; Backspace edits input. Requires a Hyprland Lua session."
        );
        return std::process::ExitCode::SUCCESS;
    }
    if args.len() == 1 && (args[0] == "--version" || args[0] == "-V") {
        println!("hyprselector {}", env!("CARGO_PKG_VERSION"));
        return std::process::ExitCode::SUCCESS;
    }
    if !args.is_empty() {
        eprintln!("hyprselector: unexpected arguments; use --help");
        return std::process::ExitCode::FAILURE;
    }
    match hyprselector::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hyprselector: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
