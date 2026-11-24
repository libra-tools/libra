-- PM-01 / GC-13: symbolic references other than HEAD.
-- `HEAD` and `refs/remotes/<remote>/HEAD` stay in the `reference` table as
-- `Head` rows; this table only stores `refs/heads/<branch>` symbolic refs and
-- the database itself keeps them mutually exclusive with a local entity branch.

CREATE TABLE IF NOT EXISTS symbolic_ref (
    name   TEXT NOT NULL PRIMARY KEY,
    target TEXT NOT NULL
);

-- A new symbolic row must not name a branch that already has an entity row.
CREATE TRIGGER IF NOT EXISTS symbolic_ref_entity_insert_guard
BEFORE INSERT ON symbolic_ref
WHEN substr(NEW.name, 1, 11) = 'refs/heads/'
BEGIN
    SELECT RAISE(ABORT, 'symbolic ref conflicts with an existing local branch row')
    WHERE EXISTS (
        SELECT 1
        FROM reference
        WHERE reference.kind = 'Branch'
          AND reference.remote IS NULL
          AND reference.name = substr(NEW.name, 12)
    );
END;

-- Renaming a symbolic row onto an entity branch is refused the same way.
CREATE TRIGGER IF NOT EXISTS symbolic_ref_entity_update_guard
BEFORE UPDATE ON symbolic_ref
WHEN substr(NEW.name, 1, 11) = 'refs/heads/'
BEGIN
    SELECT RAISE(ABORT, 'symbolic ref conflicts with an existing local branch row')
    WHERE EXISTS (
        SELECT 1
        FROM reference
        WHERE reference.kind = 'Branch'
          AND reference.remote IS NULL
          AND reference.name = substr(NEW.name, 12)
    );
END;

-- A new entity branch must not shadow an existing symbolic row.
CREATE TRIGGER IF NOT EXISTS reference_symbolic_insert_guard
BEFORE INSERT ON reference
WHEN NEW.kind = 'Branch' AND NEW.remote IS NULL AND NEW.name IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'local branch row conflicts with an existing symbolic ref')
    WHERE EXISTS (
        SELECT 1
        FROM symbolic_ref
        WHERE symbolic_ref.name = 'refs/heads/' || NEW.name
    );
END;

-- Any column update on `reference` is judged on the updated row, so promoting a
-- tag to `Branch` or renaming a branch onto a symbolic row is refused too.
CREATE TRIGGER IF NOT EXISTS reference_symbolic_update_guard
BEFORE UPDATE ON reference
WHEN NEW.kind = 'Branch' AND NEW.remote IS NULL AND NEW.name IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'local branch row conflicts with an existing symbolic ref')
    WHERE EXISTS (
        SELECT 1
        FROM symbolic_ref
        WHERE symbolic_ref.name = 'refs/heads/' || NEW.name
    );
END;
