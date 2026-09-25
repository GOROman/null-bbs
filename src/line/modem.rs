//! モデム回線 (M6 で実装)

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::config::ModemConfig;
use crate::session::Ctx;

pub async fn run(_cfg: ModemConfig, _ctx: Arc<Ctx>, _stop: CancellationToken) {}
