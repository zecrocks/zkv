use clap::Args;

use crate::{config::WalletConfig, data, ui};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// The database name to remove.
    name: String,

    /// Skip the confirmation prompt.
    #[arg(short, long)]
    yes: bool,
}

impl Command {
    pub(crate) async fn run(self, _db: Option<String>) -> anyhow::Result<()> {
        // Verify it exists.
        WalletConfig::read(&self.name)?;

        if !self.yes {
            ui::warn(format!(
                "Delete database {:?}? This removes the seed and cannot be undone.",
                self.name
            ));
            eprint!("{}", ui::bold("Type 'y' to confirm [y/N]: "));
            use std::io::{BufRead, Write};
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line)?;
            if line.trim().to_lowercase() != "y" {
                ui::failure("Aborted.");
                return Ok(());
            }
        }

        data::erase_wallet_state(&self.name).await;

        // Clear the current marker if it pointed at this db.
        if data::current_db()?.as_deref() == Some(self.name.as_str()) {
            // No "unset" function; set to empty and treat empty as None.
            let _ = std::fs::remove_file(data::zkv_data()?.join("current"));
        }

        ui::success(format!("Removed {:?}", self.name));
        Ok(())
    }
}
