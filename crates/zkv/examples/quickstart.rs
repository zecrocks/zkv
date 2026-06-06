//! Minimum-lines zkv lifecycle: create a database, broadcast INIT,
//! write a key, read it back.
//!
//! Re-runnable. The script detects which stage you're at and resumes:
//!
//! 1. First run: creates the database, prints a testnet funding
//!    address and the recovery phrase, exits. Fund the address with
//!    ~0.0001 TAZ (one Orchard fee + a little headroom).
//! 2. Second run: broadcasts INIT, exits. Wait ~75 seconds for a
//!    testnet confirmation.
//! 3. Third run: writes `hello = world`, syncs, reads it back.
//!
//! ```text
//! cargo run -p zkv --example quickstart
//! ```

use zkv::{
    data::Network,
    db::{Confirmations, Database, InitState, ZkvError},
    remote::ConnectionArgs,
};

const NAME: &str = "zkv-quickstart";

#[tokio::main]
async fn main() -> Result<(), ZkvError> {
    let conn = ConnectionArgs::default();

    // 1. Open the database, or create it on first run.
    let db = match Database::open(NAME, conn.clone()) {
        Ok(d) => d,
        Err(ZkvError::UnknownDatabase(_)) => {
            let (d, phrase) = Database::init_admin(NAME, Network::Test, conn).await?;
            eprintln!("recovery phrase: {phrase}");
            eprintln!("fund this on testnet (~0.0001 TAZ), then re-run:");
            eprintln!("  {}", d.zkv_address()?);
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    // 2. Broadcast INIT if it hasn't happened yet.
    db.sync().await?;
    if matches!(
        db.init_state(Confirmations::OneBlock)?,
        InitState::Uninitialized
    ) {
        eprintln!("INIT broadcast: {}", db.init().await?);
        eprintln!("re-run after a confirmation lands");
        return Ok(());
    }

    // 3. Write a key, sync, and read it back.
    eprintln!("SET txid: {}", db.set("hello", "world").await?);
    db.sync().await?;
    println!("hello = {:?}", db.get("hello", Confirmations::OneBlock)?);
    Ok(())
}
