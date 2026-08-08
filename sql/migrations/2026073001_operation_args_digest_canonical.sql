-- 2026073001_operation_args_digest_canonical
--
-- plan-20260714 Part C W1 (§C.9): canonicalize `operation.args_digest`.
--
-- The duplicate-suppression window is a SQL point query now — repo, worktree,
-- command, digest, status and time are all predicates — and an equality
-- comparison only works if the value written and the value searched for are
-- the same string. The writer normalizes from this release on, but rows
-- written BEFORE it may carry surrounding whitespace: a new `"digest"`
-- submission would not match a stored `" digest "`, and the duplicate it
-- should refuse would go through once, silently.
--
-- ONE normalization contract, shared with the writer
-- (`OperationMeta::normalized_digest`): strip ASCII whitespace — space, tab,
-- LF, CR, VT, FF — and treat the empty result as NO digest (NULL), which is
-- how the dedup check has always treated it (a row without a digest is never
-- a duplicate candidate). SQLite's one-argument TRIM strips only spaces,
-- which is why the character set is given explicitly; the writer trims the
-- same ASCII set rather than Rust's wider Unicode set so the two cannot
-- disagree.
--
-- Idempotent: a second run finds every value already canonical, and the
-- predicate makes it a no-op write-wise. A digest is a hex/prefixed ASCII
-- token, so no legitimate value loses information.
UPDATE `operation`
SET `args_digest` = NULLIF(
        TRIM(`args_digest`, ' ' || char(9) || char(10) || char(13) || char(11) || char(12)),
        ''
    )
WHERE `args_digest` IS NOT NULL
  AND `args_digest` IS NOT NULLIF(
        TRIM(`args_digest`, ' ' || char(9) || char(10) || char(13) || char(11) || char(12)),
        ''
      );
