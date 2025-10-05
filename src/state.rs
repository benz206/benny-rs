use parking_lot::Mutex;
use reqwest::Client as HttpClient;
use sqlx::SqlitePool;
use std::sync::Arc;
use tokio::time::{sleep, Duration};

#[derive(Clone)]
pub struct AppState {
    http: HttpClient,
    servers_db: SqlitePool,
    users_db: SqlitePool,
    latency_ms: Arc<Mutex<Vec<u64>>>,
    prefix: String,
}

impl AppState {
    pub fn new(
        http: HttpClient,
        servers_db: SqlitePool,
        users_db: SqlitePool,
        prefix: String,
    ) -> Self {
        Self {
            http,
            servers_db,
            users_db,
            latency_ms: Arc::new(Mutex::new(Vec::with_capacity(64))),
            prefix,
        }
    }

    pub fn http(&self) -> &HttpClient {
        &self.http
    }

    pub fn servers_db(&self) -> &SqlitePool {
        &self.servers_db
    }

    pub fn users_db(&self) -> &SqlitePool {
        &self.users_db
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn latency(&self) -> Arc<Mutex<Vec<u64>>> {
        self.latency_ms.clone()
    }
}

pub fn start_latency_task(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut value: u64 = 0;
        loop {
            {
                let mut history = state.latency_ms.lock();
                if history.len() >= 60 {
                    history.remove(0);
                }
                history.push(value);
            }
            value = value.saturating_add(1);
            sleep(Duration::from_secs(30)).await;
        }
    });
}


