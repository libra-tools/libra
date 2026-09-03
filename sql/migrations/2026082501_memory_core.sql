-- M2-02: core Agent Memory projections and bounded compiler observation state.
--
-- The seven projection tables are rebuildable from the repository Memory ref.
-- The job and observer tables are local coordination state. FTS, receipts,
-- replay, writer objects, and compiler execution belong to later migrations.

CREATE TABLE IF NOT EXISTS `memory_note_index` (
    `note_id` TEXT PRIMARY KEY CHECK (
        length(`note_id`) = 36
        AND substr(`note_id`, 9, 1) = '-'
        AND substr(`note_id`, 14, 1) = '-'
        AND substr(`note_id`, 19, 1) = '-'
        AND substr(`note_id`, 24, 1) = '-'
        AND length(replace(`note_id`, '-', '')) = 32
        AND replace(`note_id`, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    `scope_key` TEXT NOT NULL
        CHECK (length(CAST(`scope_key` AS BLOB)) BETWEEN 1 AND 512),
    `namespace` TEXT NOT NULL
        CHECK (length(CAST(`namespace` AS BLOB)) BETWEEN 1 AND 512),
    `path` TEXT NOT NULL
        CHECK (length(CAST(`path` AS BLOB)) BETWEEN 1 AND 4096),
    `kind` TEXT NOT NULL
        CHECK (`kind` IN ('procedural','semantic','episodic')),
    `lifecycle` TEXT NOT NULL
        CHECK (`lifecycle` IN ('replacement','accretive')),
    `review_state` TEXT NOT NULL
        CHECK (`review_state` IN ('draft','confirmed','quarantined','revoked','superseded','forgotten')),
    `confidence` TEXT NOT NULL
        CHECK (`confidence` IN ('low','medium','high')),
    `trust` TEXT NOT NULL
        CHECK (`trust` IN ('verified','repo_evidence','user_asserted','external_untrusted','inferred')),
    `sensitivity` TEXT NOT NULL
        CHECK (`sensitivity` IN ('public','internal','confidential','secret_like')),
    `visibility` TEXT NOT NULL
        CHECK (`visibility` IN ('private','repo_local','team_candidate')),
    `acl_policy_id` TEXT NOT NULL,
    `origin` TEXT NOT NULL CHECK (`origin` IN (
        'explicit','promoted_from_anchor','distilled_from_frame','classifier',
        'consolidation','onboard','branch_fork','import','coordinator','episode_compiler'
    )),
    `idempotency_key` TEXT NOT NULL,
    `idempotency_scope` TEXT NOT NULL DEFAULT 'cell'
        CHECK (`idempotency_scope` IN ('cell','namespace')),
    `created_at` TEXT NOT NULL,
    UNIQUE (`scope_key`, `namespace`, `path`, `note_id`),
    UNIQUE (`note_id`, `scope_key`, `namespace`)
);

CREATE UNIQUE INDEX IF NOT EXISTS `idx_memory_note_idempotency_cell`
    ON `memory_note_index`(`scope_key`, `namespace`, `path`, `idempotency_key`)
    WHERE `idempotency_scope` = 'cell';
CREATE UNIQUE INDEX IF NOT EXISTS `idx_memory_note_idempotency_ns`
    ON `memory_note_index`(`scope_key`, `namespace`, `idempotency_key`)
    WHERE `idempotency_scope` = 'namespace';

CREATE TABLE IF NOT EXISTS `memory_revision_index` (
    `revision_oid` TEXT PRIMARY KEY CHECK (
        length(`revision_oid`) IN (40,64)
        AND `revision_oid` NOT GLOB '*[^0-9a-f]*'
    ),
    `note_id` TEXT NOT NULL CHECK (
        length(`note_id`) = 36
        AND substr(`note_id`, 9, 1) = '-'
        AND substr(`note_id`, 14, 1) = '-'
        AND substr(`note_id`, 19, 1) = '-'
        AND substr(`note_id`, 24, 1) = '-'
        AND length(replace(`note_id`, '-', '')) = 32
        AND replace(`note_id`, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    `scope_key` TEXT NOT NULL
        CHECK (length(CAST(`scope_key` AS BLOB)) BETWEEN 1 AND 512),
    `namespace` TEXT NOT NULL
        CHECK (length(CAST(`namespace` AS BLOB)) BETWEEN 1 AND 512),
    `origin` TEXT NOT NULL CHECK (`origin` IN (
        'explicit','promoted_from_anchor','distilled_from_frame','classifier',
        'consolidation','onboard','branch_fork','import','coordinator','episode_compiler'
    )),
    `producer` TEXT NOT NULL,
    `rules_version` INTEGER NOT NULL CHECK (`rules_version` > 0),
    `prompt_version` TEXT,
    `model_id` TEXT,
    `policy_version` TEXT NOT NULL,
    `input_fingerprints_json` TEXT NOT NULL CHECK (
        json_valid(`input_fingerprints_json`)
        AND json_type(`input_fingerprints_json`) = 'array'
    ),
    `created_at` TEXT NOT NULL,
    UNIQUE (`note_id`, `revision_oid`),
    UNIQUE (`scope_key`, `namespace`, `note_id`, `revision_oid`),
    FOREIGN KEY (`note_id`, `scope_key`, `namespace`)
        REFERENCES `memory_note_index`(`note_id`, `scope_key`, `namespace`)
        ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS `idx_memory_revision_note`
    ON `memory_revision_index`(`note_id`, `created_at`, `revision_oid`);
CREATE INDEX IF NOT EXISTS `idx_memory_revision_producer`
    ON `memory_revision_index`(
        `scope_key`, `namespace`, `producer`, `prompt_version`, `model_id`, `policy_version`
    );

CREATE TABLE IF NOT EXISTS `memory_path_summary` (
    `scope_key` TEXT NOT NULL
        CHECK (length(CAST(`scope_key` AS BLOB)) BETWEEN 1 AND 512),
    `namespace` TEXT NOT NULL
        CHECK (length(CAST(`namespace` AS BLOB)) BETWEEN 1 AND 512),
    `path` TEXT NOT NULL
        CHECK (length(CAST(`path` AS BLOB)) BETWEEN 1 AND 4096),
    `confirmed_count` INTEGER NOT NULL DEFAULT 0 CHECK (`confirmed_count` >= 0),
    `quarantined_count` INTEGER NOT NULL DEFAULT 0 CHECK (`quarantined_count` >= 0),
    `child_count` INTEGER NOT NULL DEFAULT 0 CHECK (`child_count` >= 0),
    `prefix_count` INTEGER NOT NULL DEFAULT 0 CHECK (`prefix_count` >= 0),
    `preview` TEXT NOT NULL DEFAULT '',
    `last_changed_at` TEXT NOT NULL,
    PRIMARY KEY (`scope_key`, `namespace`, `path`)
);

CREATE INDEX IF NOT EXISTS `idx_memory_path_summary_prefix`
    ON `memory_path_summary`(`scope_key`, `namespace`, `path`);

CREATE TABLE IF NOT EXISTS `memory_projection_state` (
    `scope_key` TEXT PRIMARY KEY
        CHECK (length(CAST(`scope_key` AS BLOB)) BETWEEN 1 AND 512),
    `projected_ref_oid` TEXT NOT NULL CHECK (
        length(`projected_ref_oid`) IN (40,64)
        AND `projected_ref_oid` NOT GLOB '*[^0-9a-f]*'
    ),
    `last_event_seq` INTEGER NOT NULL CHECK (`last_event_seq` >= 0),
    `schema_version` INTEGER NOT NULL CHECK (`schema_version` > 0),
    `policy_version` TEXT NOT NULL,
    `rebuilt_at` INTEGER NOT NULL CHECK (`rebuilt_at` >= 0)
);

CREATE TABLE IF NOT EXISTS `memory_head` (
    `scope_key` TEXT NOT NULL
        CHECK (length(CAST(`scope_key` AS BLOB)) BETWEEN 1 AND 512),
    `namespace` TEXT NOT NULL
        CHECK (length(CAST(`namespace` AS BLOB)) BETWEEN 1 AND 512),
    `path` TEXT NOT NULL
        CHECK (length(CAST(`path` AS BLOB)) BETWEEN 1 AND 4096),
    `note_id` TEXT NOT NULL CHECK (
        length(`note_id`) = 36
        AND substr(`note_id`, 9, 1) = '-'
        AND substr(`note_id`, 14, 1) = '-'
        AND substr(`note_id`, 19, 1) = '-'
        AND substr(`note_id`, 24, 1) = '-'
        AND length(replace(`note_id`, '-', '')) = 32
        AND replace(`note_id`, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    `latest_revision_oid` TEXT NOT NULL CHECK (
        length(`latest_revision_oid`) IN (40,64)
        AND `latest_revision_oid` NOT GLOB '*[^0-9a-f]*'
    ),
    `live_revision_oid` TEXT CHECK (
        `live_revision_oid` IS NULL
        OR (length(`live_revision_oid`) IN (40,64)
            AND `live_revision_oid` NOT GLOB '*[^0-9a-f]*')
    ),
    `latest_action` TEXT NOT NULL CHECK (`latest_action` IN (
        'created','revised','confirmed','quarantined','superseded',
        'revoked','forgotten','consolidated'
    )),
    `latest_review_state` TEXT NOT NULL CHECK (
        `latest_review_state` IN ('draft','confirmed','quarantined','revoked','superseded','forgotten')
    ),
    `kind` TEXT NOT NULL
        CHECK (`kind` IN ('procedural','semantic','episodic')),
    `lifecycle` TEXT NOT NULL
        CHECK (`lifecycle` IN ('replacement','accretive')),
    `confidence` TEXT NOT NULL
        CHECK (`confidence` IN ('low','medium','high')),
    `trust` TEXT NOT NULL
        CHECK (`trust` IN ('verified','repo_evidence','user_asserted','external_untrusted','inferred')),
    `sensitivity` TEXT NOT NULL
        CHECK (`sensitivity` IN ('public','internal','confidential','secret_like')),
    `visibility` TEXT NOT NULL
        CHECK (`visibility` IN ('private','repo_local','team_candidate')),
    `acl_policy_id` TEXT NOT NULL,
    `valid_from` TEXT,
    `valid_until` TEXT,
    `effective_from_commit` TEXT CHECK (
        `effective_from_commit` IS NULL
        OR (length(`effective_from_commit`) IN (40,64)
            AND `effective_from_commit` NOT GLOB '*[^0-9a-f]*')
    ),
    `effective_until_commit` TEXT CHECK (
        `effective_until_commit` IS NULL
        OR (length(`effective_until_commit`) IN (40,64)
            AND `effective_until_commit` NOT GLOB '*[^0-9a-f]*')
    ),
    `expires_at` TEXT,
    `rank_hint` INTEGER NOT NULL DEFAULT 0,
    `last_event_seq` INTEGER NOT NULL CHECK (`last_event_seq` >= 1),
    `updated_at` TEXT NOT NULL,
    PRIMARY KEY (`scope_key`, `namespace`, `path`, `note_id`),
    UNIQUE (`note_id`),
    FOREIGN KEY (`scope_key`, `namespace`, `path`, `note_id`)
        REFERENCES `memory_note_index`(`scope_key`, `namespace`, `path`, `note_id`),
    FOREIGN KEY (`scope_key`, `namespace`, `note_id`, `latest_revision_oid`)
        REFERENCES `memory_revision_index`(`scope_key`, `namespace`, `note_id`, `revision_oid`),
    FOREIGN KEY (`scope_key`, `namespace`, `note_id`, `live_revision_oid`)
        REFERENCES `memory_revision_index`(`scope_key`, `namespace`, `note_id`, `revision_oid`)
);

CREATE INDEX IF NOT EXISTS `idx_memory_head_lookup`
    ON `memory_head`(`scope_key`, `namespace`, `path`, `latest_review_state`);
CREATE INDEX IF NOT EXISTS `idx_memory_head_path_prefix`
    ON `memory_head`(`scope_key`, `namespace`, `path`);

CREATE TABLE IF NOT EXISTS `memory_link_index` (
    `source_scope_key` TEXT NOT NULL
        CHECK (length(CAST(`source_scope_key` AS BLOB)) BETWEEN 1 AND 512),
    `source_namespace` TEXT NOT NULL
        CHECK (length(CAST(`source_namespace` AS BLOB)) BETWEEN 1 AND 512),
    `source_note_id` TEXT NOT NULL CHECK (
        length(`source_note_id`) = 36
        AND substr(`source_note_id`, 9, 1) = '-'
        AND substr(`source_note_id`, 14, 1) = '-'
        AND substr(`source_note_id`, 19, 1) = '-'
        AND substr(`source_note_id`, 24, 1) = '-'
        AND length(replace(`source_note_id`, '-', '')) = 32
        AND replace(`source_note_id`, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    `source_revision_oid` TEXT NOT NULL CHECK (
        length(`source_revision_oid`) IN (40,64)
        AND `source_revision_oid` NOT GLOB '*[^0-9a-f]*'
    ),
    `target_note_id` TEXT NOT NULL CHECK (
        length(`target_note_id`) = 36
        AND substr(`target_note_id`, 9, 1) = '-'
        AND substr(`target_note_id`, 14, 1) = '-'
        AND substr(`target_note_id`, 19, 1) = '-'
        AND substr(`target_note_id`, 24, 1) = '-'
        AND length(replace(`target_note_id`, '-', '')) = 32
        AND replace(`target_note_id`, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    `target_revision_oid` TEXT CHECK (
        `target_revision_oid` IS NULL
        OR (length(`target_revision_oid`) IN (40,64)
            AND `target_revision_oid` NOT GLOB '*[^0-9a-f]*')
    ),
    `link_kind` TEXT NOT NULL CHECK (
        `link_kind` IN ('sibling','supports','prerequisite','contradicts','supersedes')
    ),
    `source_path` TEXT NOT NULL,
    `target_path` TEXT NOT NULL,
    `evidence_refs_json` TEXT NOT NULL CHECK (
        json_valid(`evidence_refs_json`)
        AND json_type(`evidence_refs_json`) = 'array'
    ),
    `valid_from` TEXT,
    `valid_until` TEXT,
    PRIMARY KEY (`source_revision_oid`, `target_note_id`, `link_kind`),
    FOREIGN KEY (`source_scope_key`, `source_namespace`, `source_path`, `source_note_id`)
        REFERENCES `memory_note_index`(`scope_key`, `namespace`, `path`, `note_id`)
        ON DELETE CASCADE,
    FOREIGN KEY (`source_scope_key`, `source_namespace`, `source_note_id`, `source_revision_oid`)
        REFERENCES `memory_revision_index`(`scope_key`, `namespace`, `note_id`, `revision_oid`)
        ON DELETE CASCADE,
    FOREIGN KEY (`target_note_id`)
        REFERENCES `memory_note_index`(`note_id`) ON DELETE CASCADE,
    FOREIGN KEY (`target_note_id`, `target_revision_oid`)
        REFERENCES `memory_revision_index`(`note_id`, `revision_oid`)
);

CREATE INDEX IF NOT EXISTS `idx_memory_link_source`
    ON `memory_link_index`(`source_scope_key`, `source_namespace`, `source_note_id`);
CREATE INDEX IF NOT EXISTS `idx_memory_link_target`
    ON `memory_link_index`(`target_note_id`, `target_revision_oid`);

CREATE TABLE IF NOT EXISTS `memory_episode_path` (
    `note_id` TEXT NOT NULL CHECK (
        length(`note_id`) = 36
        AND substr(`note_id`, 9, 1) = '-'
        AND substr(`note_id`, 14, 1) = '-'
        AND substr(`note_id`, 19, 1) = '-'
        AND substr(`note_id`, 24, 1) = '-'
        AND length(replace(`note_id`, '-', '')) = 32
        AND replace(`note_id`, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    `revision_oid` TEXT NOT NULL CHECK (
        length(`revision_oid`) IN (40,64)
        AND `revision_oid` NOT GLOB '*[^0-9a-f]*'
    ),
    `code_path` TEXT NOT NULL CHECK (
        length(CAST(`code_path` AS BLOB)) BETWEEN 1 AND 4096
        AND substr(`code_path`, 1, 1) <> '/'
        AND instr(`code_path`, char(92)) = 0
    ),
    PRIMARY KEY (`note_id`, `revision_oid`, `code_path`),
    FOREIGN KEY (`note_id`, `revision_oid`)
        REFERENCES `memory_revision_index`(`note_id`, `revision_oid`)
        ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS `idx_memory_episode_path_code`
    ON `memory_episode_path`(`code_path`, `note_id`, `revision_oid`);

CREATE TABLE IF NOT EXISTS `memory_compile_job` (
    `scope_key` TEXT NOT NULL
        CHECK (length(CAST(`scope_key` AS BLOB)) BETWEEN 1 AND 512),
    `root_kind` TEXT NOT NULL CHECK (`root_kind` IN ('task','intent')),
    `root_id` TEXT NOT NULL CHECK (
        length(`root_id`) > 0 AND length(CAST(`root_id` AS BLOB)) <= 120
    ),
    `terminal_source_oid` TEXT NOT NULL CHECK (
        length(`terminal_source_oid`) IN (40,64)
        AND `terminal_source_oid` NOT GLOB '*[^0-9a-f]*'
    ),
    `input_fingerprint_version` INTEGER NOT NULL
        CHECK (`input_fingerprint_version` = 1),
    `input_fingerprint_key_id` TEXT NOT NULL CHECK (
        length(`input_fingerprint_key_id`) = 36
        AND substr(`input_fingerprint_key_id`, 9, 1) = '-'
        AND substr(`input_fingerprint_key_id`, 14, 1) = '-'
        AND substr(`input_fingerprint_key_id`, 19, 1) = '-'
        AND substr(`input_fingerprint_key_id`, 24, 1) = '-'
        AND length(replace(`input_fingerprint_key_id`, '-', '')) = 32
        AND replace(`input_fingerprint_key_id`, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    `input_fingerprint_digest` TEXT NOT NULL CHECK (
        length(`input_fingerprint_digest`) = 64
        AND `input_fingerprint_digest` NOT GLOB '*[^0-9a-f]*'
    ),
    `observed_generation` INTEGER NOT NULL DEFAULT 1
        CHECK (`observed_generation` >= 1),
    `processed_generation` INTEGER NOT NULL DEFAULT 0
        CHECK (`processed_generation` >= 0),
    `state` TEXT NOT NULL DEFAULT 'dirty'
        CHECK (`state` IN ('idle','dirty','inflight','failed')),
    `lease_owner` TEXT,
    `lease_fence` INTEGER NOT NULL DEFAULT 0 CHECK (`lease_fence` >= 0),
    `lease_expires_at` INTEGER,
    `retry_count` INTEGER NOT NULL DEFAULT 0 CHECK (`retry_count` >= 0),
    `next_retry_at` INTEGER,
    `last_error_code` TEXT CHECK (
        `last_error_code` IS NULL
        OR (length(`last_error_code`) = 14
            AND `last_error_code` GLOB 'LBR-MEMORY-[0-9][0-9][0-9]')
    ),
    `last_error_summary` TEXT CHECK (
        `last_error_summary` IS NULL
        OR length(CAST(`last_error_summary` AS BLOB)) <= 1024
    ),
    `created_at` INTEGER NOT NULL CHECK (`created_at` >= 0),
    `updated_at` INTEGER NOT NULL CHECK (`updated_at` >= `created_at`),
    PRIMARY KEY (`scope_key`, `root_kind`, `root_id`),
    CHECK (`processed_generation` <= `observed_generation`),
    CHECK (
        (`lease_owner` IS NULL AND `lease_expires_at` IS NULL)
        OR (`lease_owner` IS NOT NULL AND `lease_expires_at` IS NOT NULL AND `lease_fence` > 0)
    ),
    CHECK (`lease_expires_at` IS NULL OR `lease_expires_at` >= 0),
    CHECK (`next_retry_at` IS NULL OR `next_retry_at` >= 0),
    CHECK (`last_error_summary` IS NULL OR `last_error_code` IS NOT NULL),
    CHECK (
        (`state` = 'idle'
         AND `processed_generation` = `observed_generation`
         AND `lease_owner` IS NULL AND `lease_expires_at` IS NULL
         AND `retry_count` = 0 AND `next_retry_at` IS NULL
         AND `last_error_code` IS NULL AND `last_error_summary` IS NULL)
        OR
        (`state` = 'dirty'
         AND `processed_generation` < `observed_generation`
         AND `lease_owner` IS NULL AND `lease_expires_at` IS NULL)
        OR
        (`state` = 'inflight'
         AND `processed_generation` < `observed_generation`
         AND `lease_owner` IS NOT NULL AND `lease_expires_at` IS NOT NULL
         AND `next_retry_at` IS NULL)
        OR
        (`state` = 'failed'
         AND `processed_generation` < `observed_generation`
         AND `lease_owner` IS NULL AND `lease_expires_at` IS NULL
         AND `retry_count` > 0 AND `next_retry_at` IS NULL
         AND `last_error_code` IS NOT NULL)
    ),
    CHECK (`next_retry_at` IS NULL OR `state` = 'dirty'),
    CHECK (
        `retry_count` > 0
        OR (`next_retry_at` IS NULL
            AND `last_error_code` IS NULL AND `last_error_summary` IS NULL)
    )
);

CREATE INDEX IF NOT EXISTS `idx_memory_compile_job_runnable`
    ON `memory_compile_job`(`state`, `next_retry_at`, `lease_expires_at`, `updated_at`);
CREATE INDEX IF NOT EXISTS `idx_memory_compile_job_scope_generation`
    ON `memory_compile_job`(`scope_key`, `observed_generation`, `processed_generation`);

CREATE TABLE IF NOT EXISTS `memory_compile_observer_state` (
    `scope_key` TEXT NOT NULL
        CHECK (length(CAST(`scope_key` AS BLOB)) BETWEEN 1 AND 512),
    `source_ref_name` TEXT NOT NULL
        CHECK (`source_ref_name` IN ('libra/intent','libra/memory/repo')),
    `scanned_through_oid` TEXT NOT NULL CHECK (
        length(`scanned_through_oid`) IN (40,64)
        AND `scanned_through_oid` NOT GLOB '*[^0-9a-f]*'
    ),
    `updated_at` INTEGER NOT NULL CHECK (`updated_at` >= 0),
    PRIMARY KEY (`scope_key`, `source_ref_name`)
);
