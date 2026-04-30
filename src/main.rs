use std::path::PathBuf;

use anyhow::Result;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut app = opendevtui::app::App::load(workspace_root).await?;
    app.run().await
}
