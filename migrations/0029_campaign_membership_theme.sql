-- Theme column for hosted campaign memberships (Task 13).
-- The existing campaign_sessions.payload_json stores title for local-owner
-- campaigns. Hosted campaigns created through the membership service carry an
-- explicit theme_id so PlayerCharacter::instantiate_for_campaign can build a
-- runtime hero whose pins match the campaign's sealed theme.
ALTER TABLE campaign_sessions
    ADD COLUMN IF NOT EXISTS theme_id TEXT CHECK (
        theme_id IS NULL
        OR theme_id IN (
            'dev.manchester-arcana.rainbound-borough',
            'dev.manchester-arcana.emberline-archive'
        )
    );
