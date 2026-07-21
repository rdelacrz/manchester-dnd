//! Custom-action point policy and ledger types (Task 18).
//!
//! Structured actions cost zero custom-action points. A custom prompt is
//! charged only when it is accepted as a valid engine command and the
//! authoritative turn commits. Invalid input, clarification, provider
//! failure, and exact idempotent replay do not charge twice.

use serde::{Deserialize, Serialize};

/// Initial point balance granted when a character joins a campaign play session.
pub const INITIAL_ACTION_POINTS: i32 = 3;

/// Cost per accepted custom action.
pub const COST_PER_CUSTOM_ACTION: i32 = 1;

/// Maximum balance a player may hold.
pub const MAX_ACTION_POINT_BALANCE: i32 = 99;

/// Reasons for ledger entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionPointReason {
    /// Granted when a character joins a play session.
    InitialGrant,
    /// Awarded by the game master or system.
    Earned,
    /// Spent on an accepted custom action.
    CustomActionSpent,
    /// Refunded after a game-master cancellation.
    AdministrativeRefund,
}

impl ActionPointReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InitialGrant => "initial_grant",
            Self::Earned => "earned",
            Self::CustomActionSpent => "custom_action_spent",
            Self::AdministrativeRefund => "administrative_refund",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "initial_grant" => Self::InitialGrant,
            "earned" => Self::Earned,
            "custom_action_spent" => Self::CustomActionSpent,
            "administrative_refund" => Self::AdministrativeRefund,
            _ => return None,
        })
    }

    /// Returns the signed delta for this reason given an amount.
    pub fn delta(self, amount: i32) -> i32 {
        match self {
            Self::CustomActionSpent => -amount,
            _ => amount,
        }
    }
}

/// A ledger entry recording a point balance change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ActionPointLedgerEntry {
    pub account_id: String,
    pub campaign_id: String,
    pub runtime_character_id: String,
    pub play_session_id: String,
    pub turn_revision: u64,
    pub amount: i32,
    pub reason: ActionPointReason,
    pub idempotency_key: String,
    pub created_at: String,
}

/// Policy decisions for custom-action points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionPointPolicy {
    pub initial_balance: i32,
    pub cost_per_custom_action: i32,
    pub max_balance: i32,
    pub earn_rate: i32,
    pub allow_refund_after_gm_cancel: bool,
}

impl Default for ActionPointPolicy {
    fn default() -> Self {
        Self {
            initial_balance: INITIAL_ACTION_POINTS,
            cost_per_custom_action: COST_PER_CUSTOM_ACTION,
            max_balance: MAX_ACTION_POINT_BALANCE,
            earn_rate: 0,
            allow_refund_after_gm_cancel: true,
        }
    }
}

impl ActionPointPolicy {
    /// Returns the balance after applying a reason+amount, or None if it
    /// would go negative or exceed the max.
    pub fn apply(
        &self,
        current_balance: i32,
        reason: ActionPointReason,
        amount: i32,
    ) -> Option<i32> {
        let delta = reason.delta(amount);
        let new_balance = current_balance + delta;
        if new_balance < 0 {
            return None;
        }
        if new_balance > self.max_balance {
            return None;
        }
        Some(new_balance)
    }

    /// Returns true if a custom action can be spent given the current balance.
    pub fn can_spend(&self, current_balance: i32) -> bool {
        current_balance >= self.cost_per_custom_action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_grant_adds_balance() {
        let policy = ActionPointPolicy::default();
        let balance = policy.apply(0, ActionPointReason::InitialGrant, 3).unwrap();
        assert_eq!(balance, 3);
    }

    #[test]
    fn custom_action_spent_reduces_balance() {
        let policy = ActionPointPolicy::default();
        let balance = policy
            .apply(3, ActionPointReason::CustomActionSpent, 1)
            .unwrap();
        assert_eq!(balance, 2);
    }

    #[test]
    fn insufficient_balance_prevents_spend() {
        let policy = ActionPointPolicy::default();
        assert!(
            policy
                .apply(0, ActionPointReason::CustomActionSpent, 1)
                .is_none()
        );
    }

    #[test]
    fn balance_cannot_go_negative() {
        let policy = ActionPointPolicy::default();
        assert!(
            policy
                .apply(0, ActionPointReason::CustomActionSpent, 5)
                .is_none()
        );
    }

    #[test]
    fn refund_restores_balance() {
        let policy = ActionPointPolicy::default();
        let balance = policy
            .apply(2, ActionPointReason::AdministrativeRefund, 1)
            .unwrap();
        assert_eq!(balance, 3);
    }

    #[test]
    fn max_balance_caps_grant() {
        let policy = ActionPointPolicy::default();
        assert!(policy.apply(99, ActionPointReason::Earned, 1).is_none());
    }

    #[test]
    fn can_spend_checks_minimum() {
        let policy = ActionPointPolicy::default();
        assert!(policy.can_spend(1));
        assert!(policy.can_spend(3));
        assert!(!policy.can_spend(0));
    }

    #[test]
    fn reason_delta_is_signed() {
        assert_eq!(ActionPointReason::InitialGrant.delta(3), 3);
        assert_eq!(ActionPointReason::Earned.delta(1), 1);
        assert_eq!(ActionPointReason::CustomActionSpent.delta(1), -1);
        assert_eq!(ActionPointReason::AdministrativeRefund.delta(2), 2);
    }

    #[test]
    fn reason_round_trips_through_string() {
        for reason in [
            ActionPointReason::InitialGrant,
            ActionPointReason::Earned,
            ActionPointReason::CustomActionSpent,
            ActionPointReason::AdministrativeRefund,
        ] {
            assert_eq!(ActionPointReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(ActionPointReason::parse("unknown"), None);
    }
}
