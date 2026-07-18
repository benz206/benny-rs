mod commands;
mod components;
mod handlers;
mod model;

use crate::framework::{Data, Error};
use crate::state::AppState;
use dashmap::DashMap;
use model::EmbedData;
use std::sync::Arc;
use std::sync::LazyLock;

// ---- custom_id namespace --------------------------------------------------
//
// `on_component`/`on_modal` are fanned out to every cog, so every id this cog
// owns is prefixed with `emb:` and we early-return for anything else.
const ID_PREFIX: &str = "emb:";

// Buttons / selects on the builder message.
const BTN_AUTHOR: &str = "emb:author";
const BTN_BASE: &str = "emb:base";
const BTN_IMAGES: &str = "emb:images";
const BTN_FOOTER: &str = "emb:footer";
const BTN_ADDFIELD: &str = "emb:addfield";
const BTN_REMOVEFIELD: &str = "emb:removefield";
const SEL_REMOVE: &str = "emb:removeselect";
const BTN_SEND: &str = "emb:send";
const SEL_SEND: &str = "emb:sendselect";
const BTN_BACK: &str = "emb:back";
const BTN_IMPORT: &str = "emb:import";
const BTN_EXPORT_JSON: &str = "emb:exportjson";
const BTN_EXPORT_MYST: &str = "emb:exportmyst";
const BTN_CANCEL: &str = "emb:cancel";
const BTN_COMPLETE: &str = "emb:complete";

// Modal ids.
const MODAL_AUTHOR: &str = "emb:modal:author";
const MODAL_BASE: &str = "emb:modal:base";
const MODAL_IMAGES: &str = "emb:modal:images";
const MODAL_FOOTER: &str = "emb:modal:footer";
const MODAL_ADDFIELD: &str = "emb:modal:addfield";
const MODAL_IMPORT: &str = "emb:modal:import";

/// Discord hard limits (used to keep previews valid).
const MAX_FIELDS: usize = 25;

/// An interactive builder session, keyed by the builder message id. `owner_id`
/// enforces that only the invoker may drive it.
struct Builder {
    data: EmbedData,
    owner_id: u64,
}

// ---- module-level session stores ------------------------------------------
//
// Command fns are free functions and cannot see cog struct fields, so both
// session maps live here as module statics shared by command fns and the
// `on_component`/`on_modal` hooks.

static BUILDERS: LazyLock<DashMap<u64, Builder>> = LazyLock::new(DashMap::new);
/// Cap on concurrent embed-builder sessions (bounds memory if sessions are
/// abandoned without Cancel/Complete).
const MAX_BUILDERS: usize = 500;
static TEXT_SESSIONS: LazyLock<DashMap<u64, EmbedData>> = LazyLock::new(DashMap::new);

pub struct EmbedCog {
    state: Arc<AppState>,
}

impl EmbedCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![commands::embed()]
}
