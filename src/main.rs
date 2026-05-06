use std::path::PathBuf;

use anyhow::Result;
use opendevtui::cli::{help_text, parse_args, version_text, Command};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    match parse_args(std::env::args().skip(1))? {
        Command::Help => {
            print!("{}", help_text());
            return Ok(());
        }
        Command::Version => {
            println!("{}", version_text());
            return Ok(());
        }
        Command::Run => {}
    }

    let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut app = opendevtui::app::App::load(workspace_root).await?;
    app.run().await
}
