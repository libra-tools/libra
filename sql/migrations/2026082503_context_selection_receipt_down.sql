-- Rollback of 2026082503_context_selection_receipt.
--
-- A non-empty ledger or retention watermark is audit evidence. Refuse to drop
-- either and require a forward migration instead.

CREATE TABLE IF NOT EXISTS `context_selection_receipt_down_guard` (
    `blocked` INTEGER NOT NULL,
    CONSTRAINT `context_selection_receipt_down_guard_empty` CHECK (`blocked` = 0)
);

INSERT INTO `context_selection_receipt_down_guard` (`blocked`)
SELECT 1
WHERE EXISTS (SELECT 1 FROM `context_selection_receipt` LIMIT 1)
   OR EXISTS (SELECT 1 FROM `context_selection_receipt_retention` LIMIT 1);

DROP TABLE `context_selection_receipt_down_guard`;
DROP INDEX IF EXISTS `idx_context_selection_receipt_time`;
DROP INDEX IF EXISTS `idx_context_selection_receipt_repository_time`;
DROP TABLE IF EXISTS `context_selection_receipt_retention`;
DROP TABLE IF EXISTS `context_selection_receipt`;
