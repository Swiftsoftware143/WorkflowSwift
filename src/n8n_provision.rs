use sqlx::PgPool;
use uuid::Uuid;
use std::env;

/// Provision n8n resources for a new account.
/// Creates an n8n API key and stores the tenant config so the account's
/// workflows deploy to the correct n8n webhook instance with credentials.
pub async fn provision_n8n_for_account(pool: &PgPool, aid: Uuid) {
    // Generate a unique API key for this tenant's n8n instance
    let n8n_api_key = format!("ws-{}-{}", &aid.to_string()[..8], Uuid::new_v4().to_string());

    // Determine which n8n webhook URL to use (configurable via env, default to user-n8n-webhook)
    let n8n_url = env::var("N8N_WEBHOOK_URL")
        .unwrap_or_else(|_| "http://user-n8n-webhook:5679".to_string());

    // Upsert into n8n_account_config
    let result = sqlx::query(
        r#"
        INSERT INTO n8n_account_config (id, aid, n8n_url, n8n_api_key, is_active)
        VALUES ($1, $2, $3, $4, true)
        ON CONFLICT (aid) DO UPDATE SET
            n8n_url = EXCLUDED.n8n_url,
            n8n_api_key = EXCLUDED.n8n_api_key,
            is_active = true,
            updated_at = NOW()
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(&n8n_url)
    .bind(&n8n_api_key)
    .execute(pool)
    .await;

    match result {
        Ok(_) => tracing::info!(%aid, "n8n tenant config provisioned"),
        Err(e) => tracing::warn!(%aid, error = %e, "Failed to provision n8n tenant config"),
    }
}
