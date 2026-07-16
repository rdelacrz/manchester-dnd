-- The fixed one-shot MVP encounter is the complete level-1 advancement arc.
-- Preserve legacy minor claims while allowing new victories to award the
-- trusted major tier (300 XP) required for level 2.
ALTER TABLE encounter_reward_claims
    DROP CONSTRAINT encounter_reward_claims_reward_tier_check;

ALTER TABLE encounter_reward_claims
    ADD CONSTRAINT encounter_reward_claims_reward_tier_check CHECK (
        reward_tier IN ('minor', 'major')
    );
