//! Custom-action point ledger persistence (Task 18).
//!
//! The authoritative game transaction must commit the mechanical turn, point
//! spend, turn audit, and idempotency receipt together. Points are never
//! decremented in browser state or in a separate best-effort transaction.

#![allow(dead_code)]

use sqlx::PgPool;

use manchester_dnd_core::action_points::{ActionPointLedgerEntry, ActionPointReason};

use crate::repository::RepositoryError;

pub struct ActionPointRepository;

impl ActionPointRepository {
    /// Grants the initial balance when a character joins a play session.
    #[allow(clippy::too_many_arguments)]
    pub async fn grant_initial(
        pool: &PgPool,
        account_id: &str,
        campaign_id: &str,
        runtime_character_id: &str,
        play_session_id: &str,
        turn_revision: u64,
        amount: i32,
        idempotency_key: &str,
    ) -> Result<i32, RepositoryError> {
        Self::apply(
            pool,
            account_id,
            campaign_id,
            runtime_character_id,
            play_session_id,
            turn_revision,
            amount,
            ActionPointReason::InitialGrant,
            idempotency_key,
        )
        .await
    }

    /// Spends points for an accepted custom action. Returns the new balance.
    /// Returns NotFound if insufficient balance.
    #[allow(clippy::too_many_arguments)]
    pub async fn spend(
        pool: &PgPool,
        account_id: &str,
        campaign_id: &str,
        runtime_character_id: &str,
        play_session_id: &str,
        turn_revision: u64,
        amount: i32,
        idempotency_key: &str,
    ) -> Result<i32, RepositoryError> {
        Self::apply(
            pool,
            account_id,
            campaign_id,
            runtime_character_id,
            play_session_id,
            turn_revision,
            amount,
            ActionPointReason::CustomActionSpent,
            idempotency_key,
        )
        .await
    }

    /// Refunds points after a game-master cancellation.
    #[allow(clippy::too_many_arguments)]
    pub async fn refund(
        pool: &PgPool,
        account_id: &str,
        campaign_id: &str,
        runtime_character_id: &str,
        play_session_id: &str,
        turn_revision: u64,
        amount: i32,
        idempotency_key: &str,
    ) -> Result<i32, RepositoryError> {
        Self::apply(
            pool,
            account_id,
            campaign_id,
            runtime_character_id,
            play_session_id,
            turn_revision,
            amount,
            ActionPointReason::AdministrativeRefund,
            idempotency_key,
        )
        .await
    }

    /// Loads the current balance for a character in a campaign.
    pub async fn load_balance(
        pool: &PgPool,
        account_id: &str,
        campaign_id: &str,
        runtime_character_id: &str,
    ) -> Result<i32, RepositoryError> {
        let row: Option<(i32,)> = sqlx::query_as(
            "SELECT balance FROM custom_action_point_balances
             WHERE account_id = $1 AND campaign_id = $2 AND runtime_character_id = $3",
        )
        .bind(account_id)
        .bind(campaign_id)
        .bind(runtime_character_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| RepositoryError::Database(e))?;

        Ok(row.map(|(b,)| b).unwrap_or(0))
    }

    /// Core apply function: inserts a ledger entry and updates the balance
    /// atomically in a single transaction with row-level locking.
    #[allow(clippy::too_many_arguments, clippy::redundant_closure)]
    async fn apply(
        pool: &PgPool,
        account_id: &str,
        campaign_id: &str,
        runtime_character_id: &str,
        play_session_id: &str,
        turn_revision: u64,
        amount: i32,
        reason: ActionPointReason,
        idempotency_key: &str,
    ) -> Result<i32, RepositoryError> {
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| RepositoryError::Database(e))?;

        let ledger_id = format!("capl:{}", idempotency_key);

        // Insert ledger entry — idempotent on (idempotency_key, reason) conflict
        let ledger_result = sqlx::query(
            "INSERT INTO custom_action_point_ledger
             (id, account_id, campaign_id, runtime_character_id, play_session_id,
              turn_revision, amount, reason, idempotency_key)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (idempotency_key, reason) DO NOTHING",
        )
        .bind(&ledger_id)
        .bind(account_id)
        .bind(campaign_id)
        .bind(runtime_character_id)
        .bind(play_session_id)
        .bind(turn_revision as i64)
        .bind(amount)
        .bind(reason.as_str())
        .bind(idempotency_key)
        .execute(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Database(e))?;

        if ledger_result.rows_affected() == 0 {
            // Idempotent replay — return current balance without charging again
            let balance: (i32,) = sqlx::query_as(
                "SELECT balance FROM custom_action_point_balances
                 WHERE account_id = $1 AND campaign_id = $2 AND runtime_character_id = $3
                 FOR UPDATE",
            )
            .bind(account_id)
            .bind(campaign_id)
            .bind(runtime_character_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| RepositoryError::Database(e))?;

            tx.commit()
                .await
                .map_err(|e| RepositoryError::Database(e))?;
            return Ok(balance.0);
        }

        // Lock and update balance
        let delta = reason.delta(amount);

        let new_balance: i32 = match reason {
            ActionPointReason::CustomActionSpent => {
                // For spending, lock the row and verify sufficient balance
                let row: Option<(i32,)> = sqlx::query_as(
                    "SELECT balance FROM custom_action_point_balances
                     WHERE account_id = $1 AND campaign_id = $2 AND runtime_character_id = $3
                     FOR UPDATE",
                )
                .bind(account_id)
                .bind(campaign_id)
                .bind(runtime_character_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| RepositoryError::Database(e))?;

                let current = row.map(|(b,)| b).unwrap_or(0);
                if current < amount {
                    // Roll back the ledger insert by aborting the transaction
                    return Err(RepositoryError::NotFound {
                        entity: "action_point_balance",
                        id: format!("{account_id}/{campaign_id}/{runtime_character_id}"),
                    });
                }
                let new_val = current - amount;

                sqlx::query(
                    "UPDATE custom_action_point_balances
                     SET balance = $4, updated_at = CURRENT_TIMESTAMP
                     WHERE account_id = $1 AND campaign_id = $2 AND runtime_character_id = $3",
                )
                .bind(account_id)
                .bind(campaign_id)
                .bind(runtime_character_id)
                .bind(new_val)
                .execute(&mut *tx)
                .await
                .map_err(|e| RepositoryError::Database(e))?;

                new_val
            }
            _ => {
                // For grants/refunds, upsert the balance
                let row: (i32,) = sqlx::query_as(
                    "INSERT INTO custom_action_point_balances
                     (account_id, campaign_id, runtime_character_id, play_session_id, balance)
                     VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (account_id, campaign_id, runtime_character_id)
                     DO UPDATE SET balance = custom_action_point_balances.balance + EXCLUDED.balance,
                                   updated_at = CURRENT_TIMESTAMP
                     RETURNING balance",
                )
                .bind(account_id)
                .bind(campaign_id)
                .bind(runtime_character_id)
                .bind(play_session_id)
                .bind(delta)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| RepositoryError::Database(e))?;

                row.0
            }
        };

        tx.commit()
            .await
            .map_err(|e| RepositoryError::Database(e))?;

        Ok(new_balance)
    }

    /// Loads the full ledger for a play session.
    pub async fn list_ledger(
        pool: &PgPool,
        play_session_id: &str,
    ) -> Result<Vec<ActionPointLedgerEntry>, RepositoryError> {
        let rows = sqlx::query_as(
            "SELECT account_id, campaign_id, runtime_character_id, play_session_id,
                    turn_revision, amount, reason, idempotency_key, created_at::text
             FROM custom_action_point_ledger
             WHERE play_session_id = $1
             ORDER BY created_at ASC",
        )
        .bind(play_session_id)
        .fetch_all(pool)
        .await
        .map_err(|e| RepositoryError::Database(e))?;

        Ok(rows
            .into_iter()
            .map(
                |row: (
                    String,
                    String,
                    String,
                    String,
                    i64,
                    i32,
                    String,
                    String,
                    String,
                )| {
                    ActionPointLedgerEntry {
                        account_id: row.0,
                        campaign_id: row.1,
                        runtime_character_id: row.2,
                        play_session_id: row.3,
                        turn_revision: row.4 as u64,
                        amount: row.5,
                        reason: ActionPointReason::parse(&row.6)
                            .unwrap_or(ActionPointReason::Earned),
                        idempotency_key: row.7,
                        created_at: row.8,
                    }
                },
            )
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    fn account_id(n: u8) -> String {
        format!("account:test-{n:04x}")
    }

    fn character_id(n: u8) -> String {
        format!("hero:test-{n:04x}")
    }

    fn campaign_id() -> &'static str {
        "campaign:lobby-test-0001"
    }

    async fn seed(pool: &PgPool, account: &str) {
        sqlx::query("INSERT INTO accounts (id, normalized_email, display_name, login_enabled) VALUES ($1, $2, $3, false) ON CONFLICT DO NOTHING")
            .bind(account)
            .bind(format!("{account}@test.local"))
            .bind("Test Account")
            .execute(pool)
            .await
            .unwrap();
    }

    async fn seed_campaign(pool: &PgPool, campaign: &str, owner: &str) {
        sqlx::query("INSERT INTO campaign_sessions (id, title, owner_key, owner_account_id) VALUES ($1, 'Test Campaign', 'local-owner', $2) ON CONFLICT DO NOTHING")
            .bind(campaign)
            .bind(owner)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn seed_hero(pool: &PgPool, hero: &str, owner: &str) {
        sqlx::query("INSERT INTO hero_characters (id, owner_key, schema_version, campaign_session_id, character_json) VALUES ($1, $2, 1, 'campaign:lobby-test-0001', '{}') ON CONFLICT DO NOTHING")
            .bind(hero)
            .bind(owner)
            .execute(pool)
            .await
            .unwrap();
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn grant_initial_creates_balance(pool: PgPool) {
        let acct = account_id(1);
        let hero = character_id(1);
        seed(&pool, &acct).await;
        seed_campaign(&pool, campaign_id(), &acct).await;
        seed_hero(&pool, &hero, &acct).await;

        let balance = ActionPointRepository::grant_initial(
            &pool,
            &acct,
            campaign_id(),
            &hero,
            "play:1",
            1,
            3,
            "key-1",
        )
        .await
        .unwrap();
        assert_eq!(balance, 3);
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn spend_reduces_balance(pool: PgPool) {
        let acct = account_id(2);
        let hero = character_id(2);
        seed(&pool, &acct).await;
        seed_campaign(&pool, campaign_id(), &acct).await;
        seed_hero(&pool, &hero, &acct).await;

        ActionPointRepository::grant_initial(
            &pool,
            &acct,
            campaign_id(),
            &hero,
            "play:2",
            1,
            3,
            "key-2",
        )
        .await
        .unwrap();
        let balance = ActionPointRepository::spend(
            &pool,
            &acct,
            campaign_id(),
            &hero,
            "play:2",
            2,
            1,
            "key-3",
        )
        .await
        .unwrap();
        assert_eq!(balance, 2);
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn insufficient_balance_returns_not_found(pool: PgPool) {
        let acct = account_id(3);
        let hero = character_id(3);
        seed(&pool, &acct).await;
        seed_campaign(&pool, campaign_id(), &acct).await;
        seed_hero(&pool, &hero, &acct).await;

        let result = ActionPointRepository::spend(
            &pool,
            &acct,
            campaign_id(),
            &hero,
            "play:3",
            1,
            1,
            "key-4",
        )
        .await;
        assert!(matches!(result, Err(RepositoryError::NotFound { .. })));
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn idempotent_replay_returns_same_balance(pool: PgPool) {
        let acct = account_id(4);
        let hero = character_id(4);
        seed(&pool, &acct).await;
        seed_campaign(&pool, campaign_id(), &acct).await;
        seed_hero(&pool, &hero, &acct).await;

        ActionPointRepository::grant_initial(
            &pool,
            &acct,
            campaign_id(),
            &hero,
            "play:4",
            1,
            3,
            "key-5",
        )
        .await
        .unwrap();
        let b1 = ActionPointRepository::spend(
            &pool,
            &acct,
            campaign_id(),
            &hero,
            "play:4",
            2,
            1,
            "key-6",
        )
        .await
        .unwrap();
        let b2 = ActionPointRepository::spend(
            &pool,
            &acct,
            campaign_id(),
            &hero,
            "play:4",
            2,
            1,
            "key-6",
        )
        .await
        .unwrap();
        assert_eq!(b1, b2);
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn refund_restores_balance(pool: PgPool) {
        let acct = account_id(5);
        let hero = character_id(5);
        seed(&pool, &acct).await;
        seed_campaign(&pool, campaign_id(), &acct).await;
        seed_hero(&pool, &hero, &acct).await;

        ActionPointRepository::grant_initial(
            &pool,
            &acct,
            campaign_id(),
            &hero,
            "play:5",
            1,
            3,
            "key-7",
        )
        .await
        .unwrap();
        ActionPointRepository::spend(&pool, &acct, campaign_id(), &hero, "play:5", 2, 1, "key-8")
            .await
            .unwrap();
        let balance = ActionPointRepository::refund(
            &pool,
            &acct,
            campaign_id(),
            &hero,
            "play:5",
            3,
            1,
            "key-9",
        )
        .await
        .unwrap();
        assert_eq!(balance, 3);
    }

    #[sqlx::test(migrator = "crate::repository::MIGRATOR")]
    async fn load_balance_returns_zero_for_nothing(pool: PgPool) {
        let acct = account_id(6);
        let hero = character_id(6);
        seed(&pool, &acct).await;
        seed_campaign(&pool, campaign_id(), &acct).await;
        seed_hero(&pool, &hero, &acct).await;

        let balance = ActionPointRepository::load_balance(&pool, &acct, campaign_id(), &hero)
            .await
            .unwrap();
        assert_eq!(balance, 0);
    }
}
