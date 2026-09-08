-- M2-02R: shared, local-only context selection receipt ledger.
--
-- ReceiptStore is the only writer. The ledger is append-only except for its
-- bounded retention prune, which updates the per-repository watermark in the
-- same short transaction.

CREATE TABLE IF NOT EXISTS `context_selection_receipt` (
    `receipt_id` TEXT PRIMARY KEY CHECK (
        length(`receipt_id`) = 36
        AND substr(`receipt_id`, 9, 1) = '-'
        AND substr(`receipt_id`, 14, 1) = '-'
        AND substr(`receipt_id`, 15, 1) = '7'
        AND substr(`receipt_id`, 19, 1) = '-'
        AND substr(`receipt_id`, 20, 1) IN ('8', '9', 'a', 'b')
        AND substr(`receipt_id`, 24, 1) = '-'
        AND length(replace(`receipt_id`, '-', '')) = 32
        AND replace(`receipt_id`, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    `schema_version` INTEGER NOT NULL CHECK (`schema_version` = 1),
    `source_kind` TEXT NOT NULL CHECK (`source_kind` IN ('memory', 'intent', 'hook')),
    `repository_id` TEXT NOT NULL CHECK (
        length(CAST(`repository_id` AS BLOB)) BETWEEN 1 AND 512
        AND trim(`repository_id`) = `repository_id`
    ),
    `digest_key_id` TEXT NOT NULL CHECK (
        length(`digest_key_id`) = 36
        AND substr(`digest_key_id`, 9, 1) = '-'
        AND substr(`digest_key_id`, 14, 1) = '-'
        AND substr(`digest_key_id`, 15, 1) = '4'
        AND substr(`digest_key_id`, 19, 1) = '-'
        AND substr(`digest_key_id`, 20, 1) IN ('8', '9', 'a', 'b')
        AND substr(`digest_key_id`, 24, 1) = '-'
        AND length(replace(`digest_key_id`, '-', '')) = 32
        AND replace(`digest_key_id`, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    `principal_hmac` TEXT NOT NULL CHECK (
        length(`principal_hmac`) = 113
        AND substr(`principal_hmac`, 1, 12) = 'hmac-sha256:'
        AND substr(`principal_hmac`, 13, 36) = `digest_key_id`
        AND substr(`principal_hmac`, 49, 1) = ':'
        AND substr(`principal_hmac`, 50) NOT GLOB '*[^0-9a-f]*'
    ),
    `query_hmac` TEXT NOT NULL CHECK (
        length(`query_hmac`) = 113
        AND substr(`query_hmac`, 1, 12) = 'hmac-sha256:'
        AND substr(`query_hmac`, 13, 36) = `digest_key_id`
        AND substr(`query_hmac`, 49, 1) = ':'
        AND substr(`query_hmac`, 50) NOT GLOB '*[^0-9a-f]*'
    ),
    `effective_at` TEXT NOT NULL CHECK (
        length(CAST(`effective_at` AS BLOB)) BETWEEN 20 AND 40
    ),
    `code_commit` TEXT CHECK (
        `code_commit` IS NULL OR (
            length(`code_commit`) IN (40, 64)
            AND `code_commit` NOT GLOB '*[^0-9a-f]*'
        )
    ),
    `full_branch_ref` TEXT CHECK (
        `full_branch_ref` IS NULL OR (
            length(CAST(`full_branch_ref` AS BLOB)) BETWEEN 12 AND 4096
            AND substr(`full_branch_ref`, 1, 11) = 'refs/heads/'
        )
    ),
    `source_heads_json` TEXT NOT NULL CHECK (
        length(CAST(`source_heads_json` AS BLOB)) BETWEEN 2 AND 65536
        AND json_valid(`source_heads_json`)
        AND json_type(`source_heads_json`) = 'object'
    ),
    `projection_watermarks_json` TEXT NOT NULL CHECK (
        length(CAST(`projection_watermarks_json` AS BLOB)) BETWEEN 2 AND 65536
        AND json_valid(`projection_watermarks_json`)
        AND json_type(`projection_watermarks_json`) = 'object'
    ),
    `policy_hash` TEXT NOT NULL CHECK (
        length(`policy_hash`) = 71
        AND substr(`policy_hash`, 1, 7) = 'sha256:'
        AND substr(`policy_hash`, 8) NOT GLOB '*[^0-9a-f]*'
    ),
    `selector_version` TEXT NOT NULL CHECK (
        length(CAST(`selector_version` AS BLOB)) BETWEEN 1 AND 128
        AND trim(`selector_version`) = `selector_version`
    ),
    `token_budget` INTEGER NOT NULL CHECK (`token_budget` >= 0),
    `selected_json` TEXT NOT NULL CHECK (
        length(CAST(`selected_json` AS BLOB)) BETWEEN 2 AND 1048576
        AND json_valid(`selected_json`)
        AND json_type(`selected_json`) = 'array'
    ),
    `omissions_json` TEXT NOT NULL CHECK (
        length(CAST(`omissions_json` AS BLOB)) BETWEEN 2 AND 65536
        AND json_valid(`omissions_json`)
        AND json_type(`omissions_json`) = 'array'
    ),
    `bundle_hash` TEXT NOT NULL CHECK (
        length(`bundle_hash`) = 71
        AND substr(`bundle_hash`, 1, 7) = 'sha256:'
        AND substr(`bundle_hash`, 8) NOT GLOB '*[^0-9a-f]*'
    ),
    `reproducibility_state` TEXT NOT NULL CHECK (
        `reproducibility_state` IN ('reproducible', 'stale', 'expired', 'non_reproducible')
    ),
    `frame_id` TEXT CHECK (
        `frame_id` IS NULL OR (
            length(`frame_id`) = 36
            AND substr(`frame_id`, 9, 1) = '-'
            AND substr(`frame_id`, 14, 1) = '-'
            AND substr(`frame_id`, 19, 1) = '-'
            AND substr(`frame_id`, 24, 1) = '-'
            AND length(replace(`frame_id`, '-', '')) = 32
            AND replace(`frame_id`, '-', '') NOT GLOB '*[^0-9a-f]*'
        )
    ),
    `recorded_at` TEXT NOT NULL CHECK (
        length(CAST(`recorded_at` AS BLOB)) BETWEEN 20 AND 40
    )
);

CREATE INDEX IF NOT EXISTS `idx_context_selection_receipt_repository_time`
    ON `context_selection_receipt` (`repository_id`, `recorded_at`, `receipt_id`);

CREATE INDEX IF NOT EXISTS `idx_context_selection_receipt_time`
    ON `context_selection_receipt` (`recorded_at`, `receipt_id`);

CREATE TABLE IF NOT EXISTS `context_selection_receipt_retention` (
    `repository_id` TEXT PRIMARY KEY CHECK (
        length(CAST(`repository_id` AS BLOB)) BETWEEN 1 AND 512
        AND trim(`repository_id`) = `repository_id`
    ),
    `pruned_before` TEXT CHECK (
        `pruned_before` IS NULL
        OR length(CAST(`pruned_before` AS BLOB)) BETWEEN 20 AND 40
    ),
    `last_pruned_at` TEXT CHECK (
        `last_pruned_at` IS NULL
        OR length(CAST(`last_pruned_at` AS BLOB)) BETWEEN 20 AND 40
    ),
    `retained_rows` INTEGER NOT NULL DEFAULT 0 CHECK (
        `retained_rows` BETWEEN 0 AND 10000
    )
);
