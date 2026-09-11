//! Write several key/value ops in ONE transaction, via `Database::write_many`.
//!
//! A batch pays one ZIP-317 fee and produces one txid, however many ops it
//! carries: each op becomes its own zero-value memo output on the same
//! transaction. Ops apply in order, so two writes to the same key inside one
//! batch take consecutive replay versions and the later one wins.
//!
//! There is no CLI or GUI surface for batching, so this example is also how
//! the regtest harness exercises it (it runs this binary as a subprocess and
//! then reads the results back through the `zkv` CLI).
//!
//! Reads the database name from `$ZKV_DB` and the ops from `$ZKV_BATCH`, a
//! `key=value` list separated by `;`. A pair with no `=` is a DEL. Unlike the
//! other examples this also honours `$ZKV_SERVER` (a `host:port`, or an
//! operator name like `zecrocks`), because the default picks a public
//! operator, which is wrong for a local or regtest node.
//!
//! ```text
//! ZKV_DB=mydb ZKV_BATCH='a=1;b=2;a=3;old' \
//!   cargo run -p zcash_zkv --example batch_write
//! ```
//!
//! Prints the single txid to stdout.

use std::env;

use zkv::{
    db::{install_default_subscriber, Database, WriteOp},
    remote::{ConnectionArgs, Servers},
};

/// Parse the `;`-separated op list. `k=v` is a SET (values may contain `=`,
/// since only the first one splits); a bare `k` is a DEL. Empty segments are
/// skipped so a trailing `;` is harmless.
fn parse_ops(spec: &str) -> anyhow::Result<Vec<WriteOp>> {
    let ops: Vec<WriteOp> = spec
        .split(';')
        .filter(|seg| !seg.is_empty())
        .map(|seg| match seg.split_once('=') {
            Some((key, value)) => WriteOp::set(key, value),
            None => WriteOp::del(seg),
        })
        .collect();
    if ops.is_empty() {
        anyhow::bail!("$ZKV_BATCH described no ops");
    }
    Ok(ops)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_default_subscriber();

    let db_name = env::var("ZKV_DB").map_err(|_| anyhow::anyhow!("set $ZKV_DB"))?;
    let spec = env::var("ZKV_BATCH").map_err(|_| anyhow::anyhow!("set $ZKV_BATCH"))?;
    let ops = parse_ops(&spec)?;

    let mut conn = ConnectionArgs::default();
    if let Ok(server) = env::var("ZKV_SERVER") {
        conn.server = Servers::parse(&server)?;
    }

    let db = Database::open(&db_name, conn)?;
    let txid = db.write_many(&ops).await?;
    println!("{txid}");
    Ok(())
}
