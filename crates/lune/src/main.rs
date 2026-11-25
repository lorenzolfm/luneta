use std::process::ExitCode;

fn main() -> ExitCode {
    if let Err(why) = luned::tasks::start() {
        eprintln!("lune: {why}");
        return ExitCode::FAILURE;
    }
    lune::main()
}
