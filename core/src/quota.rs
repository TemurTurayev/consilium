use crate::event::Provider;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

/// The rolling usage window, in seconds (5 hours) — the period quota totals are
/// reported over. Single source of truth for the MCP `quota_status` tool and the
/// server's `/api/quota` endpoint.
pub const WINDOW_SECS: i64 = 5 * 3600;

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before unix epoch")
        .as_secs() as i64
}

/// SQLite-backed quota store.
///
/// The `Connection` is wrapped in a `Mutex` so `QuotaStore` is `Sync` — it can be
/// shared as `Arc<QuotaStore>` across the MCP server's concurrent tool calls (M3)
/// and held by reference across `.await` points in `Send` futures. Reads/writes
/// serialize on the mutex; fine for the per-run usage-log workload.
pub struct QuotaStore {
    conn: Mutex<Connection>,
}

impl QuotaStore {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS usage_log (
                id INTEGER PRIMARY KEY,
                ts INTEGER NOT NULL,
                provider TEXT NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                estimated INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;
        // Migrate DBs created before the `estimated` column. CREATE TABLE IF NOT
        // EXISTS won't alter an existing table, so add it best-effort and ignore
        // the "duplicate column name" error on already-migrated DBs.
        let _ = conn.execute(
            "ALTER TABLE usage_log ADD COLUMN estimated INTEGER NOT NULL DEFAULT 0",
            [],
        );
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_provider_ts ON usage_log(provider, ts)",
            [],
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Record CLI-measured usage (the common path for Claude/Codex).
    pub fn record(
        &self,
        provider: Provider,
        input_tokens: u64,
        output_tokens: u64,
    ) -> anyhow::Result<()> {
        self.record_at(provider, input_tokens, output_tokens, unix_now())
    }

    pub fn record_at(
        &self,
        provider: Provider,
        input_tokens: u64,
        output_tokens: u64,
        ts: i64,
    ) -> anyhow::Result<()> {
        self.record_at_flagged(provider, input_tokens, output_tokens, ts, false)
    }

    /// Record a HEURISTIC estimate (see [`crate::tokenizer`]) for providers whose
    /// CLI reports no usage (e.g. Gemini via `agy`). Flagged `estimated` so
    /// reporting can separate measured from estimated quota; it still counts in
    /// `totals_since` so least-loaded routing isn't blind to the provider.
    pub fn record_estimated(
        &self,
        provider: Provider,
        input_tokens: u64,
        output_tokens: u64,
    ) -> anyhow::Result<()> {
        self.record_at_flagged(provider, input_tokens, output_tokens, unix_now(), true)
    }

    fn record_at_flagged(
        &self,
        provider: Provider,
        input_tokens: u64,
        output_tokens: u64,
        ts: i64,
        estimated: bool,
    ) -> anyhow::Result<()> {
        let input_i64 = i64::try_from(input_tokens)
            .map_err(|_| anyhow::anyhow!("input_tokens overflows i64: {input_tokens}"))?;
        let output_i64 = i64::try_from(output_tokens)
            .map_err(|_| anyhow::anyhow!("output_tokens overflows i64: {output_tokens}"))?;
        self.conn.lock().unwrap().execute(
            "INSERT INTO usage_log (ts, provider, input_tokens, output_tokens, estimated) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![ts, provider.as_str(), input_i64, output_i64, estimated as i64],
        )?;
        tracing::debug!(
            provider = provider.as_str(),
            input_tokens,
            output_tokens,
            estimated,
            "usage recorded"
        );
        Ok(())
    }

    /// Sum of (input, output) tokens for a provider since the given unix
    /// timestamp — the COMBINED figure (CLI-measured + estimated) used for
    /// least-loaded routing.
    pub fn totals_since(&self, provider: Provider, since_unix: i64) -> anyhow::Result<(u64, u64)> {
        let (input, output): (i64, i64) = self.conn.lock().unwrap().query_row(
            "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0)
             FROM usage_log WHERE provider = ?1 AND ts >= ?2",
            rusqlite::params![provider.as_str(), since_unix],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if input < 0 || output < 0 {
            anyhow::bail!("usage_log contains negative token sums; db may be corrupt");
        }
        Ok((input as u64, output as u64))
    }

    /// Sum of (input, output) tokens RECORDED AS ESTIMATES (via
    /// [`record_estimated`](Self::record_estimated), e.g. Gemini through `agy`)
    /// for a provider since `since_unix`. Lets reporting flag heuristic figures
    /// as not directly comparable to CLI-measured usage. A provider with
    /// `estimated_totals_since == totals_since` is fully estimated.
    pub fn estimated_totals_since(
        &self,
        provider: Provider,
        since_unix: i64,
    ) -> anyhow::Result<(u64, u64)> {
        let (input, output): (i64, i64) = self.conn.lock().unwrap().query_row(
            "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0)
             FROM usage_log WHERE provider = ?1 AND ts >= ?2 AND estimated = 1",
            rusqlite::params![provider.as_str(), since_unix],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if input < 0 || output < 0 {
            anyhow::bail!("usage_log contains negative token sums; db may be corrupt");
        }
        Ok((input as u64, output as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Provider;

    #[test]
    fn records_and_aggregates_usage() {
        let store = QuotaStore::open_in_memory().unwrap();
        store.record(Provider::Gemini, 100, 20).unwrap();
        store.record(Provider::Gemini, 50, 10).unwrap();
        store.record(Provider::Codex, 7, 3).unwrap();
        let (input, output) = store.totals_since(Provider::Gemini, 0).unwrap();
        assert_eq!((input, output), (150, 30));
        let (input, output) = store.totals_since(Provider::Codex, 0).unwrap();
        assert_eq!((input, output), (7, 3));
    }

    #[test]
    fn window_excludes_old_rows() {
        let store = QuotaStore::open_in_memory().unwrap();
        let now = unix_now();
        store
            .record_at(Provider::Claude, 1000, 500, now - 10_000)
            .unwrap();
        store.record_at(Provider::Claude, 10, 5, now).unwrap();
        let (input, output) = store.totals_since(Provider::Claude, now - 3600).unwrap();
        assert_eq!((input, output), (10, 5));
    }

    #[test]
    fn empty_store_returns_zero() {
        let store = QuotaStore::open_in_memory().unwrap();
        assert_eq!(store.totals_since(Provider::Claude, 0).unwrap(), (0, 0));
    }

    #[test]
    fn estimated_records_count_toward_totals() {
        // Estimated usage (e.g. agy) must be visible to totals_since so quota
        // accounting and least-loaded routing aren't blind to the provider.
        let store = QuotaStore::open_in_memory().unwrap();
        store.record_estimated(Provider::Gemini, 100, 20).unwrap();
        store.record(Provider::Gemini, 5, 1).unwrap();
        assert_eq!(store.totals_since(Provider::Gemini, 0).unwrap(), (105, 21));
    }

    #[test]
    fn estimated_totals_isolate_estimated_rows() {
        // totals_since reports combined; estimated_totals_since isolates the
        // heuristic rows so reporting can flag them as not measured.
        let store = QuotaStore::open_in_memory().unwrap();
        store.record(Provider::Gemini, 10, 2).unwrap(); // measured
        store.record_estimated(Provider::Gemini, 100, 20).unwrap(); // estimated
        assert_eq!(store.totals_since(Provider::Gemini, 0).unwrap(), (110, 22));
        assert_eq!(
            store.estimated_totals_since(Provider::Gemini, 0).unwrap(),
            (100, 20)
        );
        // A provider with only measured usage reports zero estimated.
        store.record(Provider::Codex, 7, 3).unwrap();
        assert_eq!(
            store.estimated_totals_since(Provider::Codex, 0).unwrap(),
            (0, 0)
        );
    }
}
