use crate::cogs::Cog;
use crate::state::AppState;
use async_trait::async_trait;
use std::sync::Arc;

/// Music playback cog (Lavalink). Placeholder — implemented in the Music wave.
pub struct MusicCog {
    #[allow(dead_code)]
    state: Arc<AppState>,
}

impl MusicCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for MusicCog {}
