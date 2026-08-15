use biject::cli::{dispatch, Cli};

use clap::Parser;
use colored::Colorize;

fn main() {
    if let Err(e) = dispatch(Cli::parse().command) {
        eprintln!("{} {}", "error:".red().bold(), e);
        std::process::exit(1);
    }
}
