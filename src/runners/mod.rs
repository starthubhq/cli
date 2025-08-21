pub mod github;
pub mod local;

use anyhow::Result;

#[derive(Clone, Debug)]
pub struct DeployCtx {
    pub action: String,
    pub env: Option<String>,
    // filled by prepare()
    pub owner: Option<String>,
    pub repo: Option<String>,
}


#[async_trait::async_trait]
pub trait Runner {
    fn name(&self) -> &'static str;
    async fn ensure_auth(&self) -> Result<()>;
    async fn prepare(&self, ctx: &mut DeployCtx) -> Result<()>;
    async fn put_files(&self, ctx: &DeployCtx) -> Result<()>;
    async fn set_secrets(&self, ctx: &DeployCtx) -> Result<()>;
    async fn dispatch(&self, ctx: &DeployCtx) -> Result<()>;
}

