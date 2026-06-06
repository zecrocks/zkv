//! Set a key to a value in a zkv admin database.
//!
//! Reads name from `$ZKV_DB`, key from `$ZKV_KEY`, and value from
//! `$ZKV_VALUE`. The named database must be an admin database whose
//! INIT memo has confirmed.
//!
//! ```text
//! ZKV_DB=mydb ZKV_KEY=hello ZKV_VALUE=world \
//!   cargo run -p zkv --example set_value
//! ```

use std::env;

use zkv::{
    db::{install_default_subscriber, Database},
    remote::ConnectionArgs,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_default_subscriber();

    let db_name = env::var("ZKV_DB").map_err(|_| anyhow::anyhow!("set $ZKV_DB"))?;
    let key = env::var("ZKV_KEY").map_err(|_| anyhow::anyhow!("set $ZKV_KEY"))?;
    let value = env::var("ZKV_VALUE").map_err(|_| anyhow::anyhow!("set $ZKV_VALUE"))?;

    let db = Database::open(&db_name, ConnectionArgs::default())?;
    let txid = db.set(&key, &value).await?;
    println!("{txid}");
    Ok(())
}
