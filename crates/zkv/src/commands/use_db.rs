use clap::Args;

use crate::{config::WalletConfig, data, ui};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// The database name to switch to.
    name: String,
}

impl Command {
    pub(crate) fn run(self, _db: Option<String>) -> anyhow::Result<()> {
        // Verify it exists.
        WalletConfig::read(&self.name)?;
        data::set_current_db(&self.name)?;
        ui::success(format!("Current database is now {:?}", self.name));
        Ok(())
    }
}
