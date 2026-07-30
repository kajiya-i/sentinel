//! `sentinel` — the Sentinel CLI. Composes the concrete adapters (`sentinel-browser`,
//! `sentinel-ai`, `sentinel-store`) into the check pipeline and dispatches subcommands.
//!
//! M1 walking skeleton: only `check` (one hardcoded abnormal-state case) is wired. The full
//! clap CLI (`check` / `run` / `eval`, flags, JSON reports) is M5 (T-M5-01); until then a
//! minimal hand-rolled dispatch keeps the dependency surface small.

mod check;
mod logging;

/// Subcommand parsed from the process arguments.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    /// Run the hardcoded walking-skeleton check.
    Check,
    /// Unknown/absent subcommand — print usage.
    Usage,
}

/// Parse the first CLI argument (already stripped of the program name) into a [`Command`].
fn parse_command(args: &[String]) -> Command {
    match args.first().map(String::as_str) {
        Some("check") => Command::Check,
        _ => Command::Usage,
    }
}

#[tokio::main]
async fn main() {
    logging::init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "sentinel starting");

    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match parse_command(&args) {
        Command::Check => check::run_and_report().await,
        Command::Usage => {
            eprintln!("usage: sentinel check");
            2
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_command_should_recognize_check() {
        assert_eq!(parse_command(&args(&["check"])), Command::Check);
    }

    #[test]
    fn parse_command_should_default_to_usage() {
        assert_eq!(parse_command(&args(&[])), Command::Usage);
        assert_eq!(parse_command(&args(&["bogus"])), Command::Usage);
    }
}
