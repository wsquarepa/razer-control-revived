use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("ci") => run_ci(),
        _ => {
            eprintln!("usage: cargo xtask ci");
            ExitCode::FAILURE
        }
    }
}

/// Runs the CI gate: rustfmt check, clippy with warnings denied, then tests.
/// Fail-fast: the first failing step's exit status becomes the process status.
fn run_ci() -> ExitCode {
    let cargo: String = std::env::var("CARGO").unwrap_or_else(|_| String::from("cargo"));
    let steps: [&[&str]; 3] = [
        &["fmt", "--all", "--", "--check"],
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        &["test", "--workspace"],
    ];
    for args in steps {
        println!("$ {cargo} {}", args.join(" "));
        let status = match Command::new(&cargo).args(args).status() {
            Ok(status) => status,
            Err(error) => {
                eprintln!("failed to run `{cargo} {}`: {error}", args.join(" "));
                return ExitCode::FAILURE;
            }
        };
        if !status.success() {
            let code: u8 = status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .unwrap_or(1);
            return ExitCode::from(code);
        }
    }
    ExitCode::SUCCESS
}
