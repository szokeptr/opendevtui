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
        Command::McpInstall(args) => {
            return opendevtui::install::run(args);
        }
        Command::Run { headless } => {
            let workspace_root = std::env::current_dir()
                .and_then(|path| path.canonicalize())
                .unwrap_or_else(|_| PathBuf::from("."));
            let mut app = opendevtui::app::App::load(workspace_root).await?;
            app.enable_api().await?;
            return if headless {
                app.run_headless().await
            } else {
                app.run().await
            };
        }
    }
}
