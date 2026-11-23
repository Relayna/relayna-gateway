ALTER TABLE openai_route_settings
    DROP CONSTRAINT openai_route_settings_route_id_check,
    DROP CONSTRAINT openai_route_settings_route_check,
    ADD COLUMN inherits_wildcard BOOLEAN NOT NULL DEFAULT false,
    ADD CONSTRAINT openai_route_settings_route_id_check
        CHECK (route_id IN ('chat-completions', 'responses', 'embeddings', 'rerank', 'audio-transcriptions')),
    ADD CONSTRAINT openai_route_settings_route_check
        CHECK (route IN ('/v1/chat/completions', '/v1/responses', '/v1/embeddings', '/v1/rerank', '/v1/audio/transcriptions')),
    ADD CONSTRAINT openai_route_settings_audio_mode_check
        CHECK (route_id <> 'audio-transcriptions' OR mode = 'direct_litellm_passthrough'),
    ADD CONSTRAINT openai_route_settings_inherits_wildcard_check
        CHECK (NOT inherits_wildcard OR (route_id = 'audio-transcriptions' AND NOT enabled));

-- Preserve existing wildcard traffic until an operator sets a dedicated state.
INSERT INTO openai_route_settings (route_id, route, enabled, mode, inherits_wildcard)
VALUES ('audio-transcriptions', '/v1/audio/transcriptions', false, 'direct_litellm_passthrough', true);
