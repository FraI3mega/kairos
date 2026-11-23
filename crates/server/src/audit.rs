use anyhow::Result;
use serde_json::Value;

/// Audit rows are written in the same transaction as the action they record.
pub async fn log(tx: &tokio_postgres::Transaction<'_>, actor: Option<i64>, action: &str, target: Option<i64>, detail: Value) -> Result<()> {
    tx.execute(
        "INSERT INTO audit_log (actor_id, action, target_user_id, detail) VALUES ($1, $2, $3, $4)",
        &[&actor, &action, &target, &detail],
    )
    .await?;
    Ok(())
}
