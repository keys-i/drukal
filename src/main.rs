use std::process::ExitCode;

fn main() -> ExitCode {
    match drukal::cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => drukal::cli::error_exit(error),
    }
}
