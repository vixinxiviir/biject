//! Guards the public contract that `biject::cli::Commands` can be embedded in a
//! downstream binary's own command enum via `#[command(flatten)]`, so that a
//! separate distribution can add commands without duplicating these argument
//! definitions.
//!
//! If this stops compiling, the free command surface has changed shape in a way
//! that breaks downstream consumers.

use biject::cli::Commands;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "biject")]
struct ProCli {
    #[command(subcommand)]
    command: ProCommands,
}

#[derive(Subcommand)]
enum ProCommands {
    #[command(flatten)]
    Free(Commands),
    Migrate {
        #[arg(short, long)]
        source: String,
        #[arg(short, long)]
        target: String,
        #[arg(long)]
        out: String,
    },
}

#[test]
fn free_and_paid_commands_share_one_surface() {
    let cli = ProCli::try_parse_from(["biject", "schema", "--source", "a.csv", "--target", "b.csv"])
        .expect("free subcommand should parse");
    assert!(matches!(cli.command, ProCommands::Free(Commands::Schema { .. })));

    let cli = ProCli::try_parse_from([
        "biject", "data", "--source", "a.csv", "--target", "b.csv", "--key", "id",
    ])
    .expect("free data subcommand should parse");
    assert!(matches!(cli.command, ProCommands::Free(Commands::Data { .. })));

    let cli = ProCli::try_parse_from([
        "biject", "migrate", "--source", "dev", "--target", "prod", "--out", "up.sql",
    ])
    .expect("paid subcommand should parse");
    assert!(matches!(cli.command, ProCommands::Migrate { .. }));
}
