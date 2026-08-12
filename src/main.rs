use std::process::ExitCode;

use clap::Parser;
use v8scope::cli::{Cli, Command};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Diagnose(args) => {
            v8scope::run::execute(v8scope::contract::Mode::Diagnose, args).await
        }
        Command::Cpu(args) => v8scope::run::execute(v8scope::contract::Mode::Cpu, args).await,
        Command::Heap(args) => v8scope::run::execute(v8scope::contract::Mode::Heap, args).await,
        Command::Async(args) => v8scope::run::execute(v8scope::contract::Mode::Async, args).await,
        Command::All(args) => v8scope::run::execute(v8scope::contract::Mode::All, args).await,
        Command::Attach(args) => v8scope::attach::execute(args).await,
        Command::Analyze(args) => {
            v8scope::analyze::execute(&args.run_directory, !args.no_report).await
        }
        Command::Open(args) => v8scope::report::open(&args.run_directory),
        Command::Compare(args) => v8scope::compare::execute(args).await,
        Command::Clean(args) => v8scope::run::clean(args).await,
        Command::Schema(args) => v8scope::contract::write_schema(args.output.as_deref()),
    };

    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("v8scope: {error:#}");
            ExitCode::from(70)
        }
    }
}
