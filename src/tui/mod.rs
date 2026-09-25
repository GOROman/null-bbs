//! 管理コンソール (M5 で実装)

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::session::Ctx;

pub fn run(_ctx: Arc<Ctx>, _stop: CancellationToken) -> anyhow::Result<()> {
    Ok(())
}
