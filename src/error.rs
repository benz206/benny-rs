use thiserror::Error;

#[derive(Error, Debug)]
pub enum BotError {
    #[error("Missing permission: {0}")]
    MissingPermission(String),
    #[error("Bot is missing permission: {0}")]
    BotMissingPermission(String),
    #[error("Command on cooldown, retry in {seconds}s")]
    Cooldown { seconds: u64 },
    #[error("Member not found")]
    MemberNotFound,
    #[error("Bad argument: {0}")]
    BadArgument(String),
    #[error("Music error: {0}")]
    Music(String),
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Unexpected error: {0}")]
    Unexpected(#[from] anyhow::Error),
}

pub type BotResult<T> = std::result::Result<T, BotError>;
