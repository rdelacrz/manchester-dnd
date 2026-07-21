-- Player character audits must survive character deletion so that the audit
-- trail is preserved for historical accountability. The original migration
-- (0026) used ON DELETE CASCADE, which silently destroyed audit rows when a
-- character was deleted. This migration drops the cascade constraint and
-- replaces it with ON DELETE SET NULL so audits persist after deletion.

-- Also relax the command_receipts FK: draft operations write receipts scoped
-- to draft IDs, which do not exist in player_characters. Drop the FK and
-- re-add without it so both character: and draft: IDs are accepted.

ALTER TABLE player_character_audits
    DROP CONSTRAINT IF EXISTS player_character_audits_character_id_fkey;

ALTER TABLE player_character_audits
    ALTER COLUMN character_id DROP NOT NULL;

ALTER TABLE player_character_audits
    ADD CONSTRAINT player_character_audits_character_id_fkey
    FOREIGN KEY (character_id) REFERENCES player_characters(id) ON DELETE SET NULL;

ALTER TABLE player_character_command_receipts
    DROP CONSTRAINT IF EXISTS player_character_command_receipts_character_id_fkey;
