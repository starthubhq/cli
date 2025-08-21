use anyhow::{Result, bail};
use super::{Runner, DeployCtx};

pub struct LocalRunner;

#[async_trait::async_trait]
impl Runner for LocalRunner {
    fn name(&self) -> &'static str { "local" }

    async fn ensure_auth(&self) -> Result<()> {
        // No auth needed for local runner (for now)
        Ok(())
    }

    async fn prepare(&self, _ctx: &mut DeployCtx) -> Result<()> { Ok(()) }
    async fn put_files(&self, _ctx: &DeployCtx) -> Result<()> { Ok(()) }
    async fn set_secrets(&self, _ctx: &DeployCtx) -> Result<()> { Ok(()) }

    async fn dispatch(&self, _ctx: &DeployCtx) -> Result<()> {
        bail!("--runner local not implemented yet")
    }
}
