-- Converge 10.11.8/10.11.9 and existing Ferrofin databases to Jellyfin
-- 10.11.10/10.11.11's username schema. Fully migrated newer Jellyfin databases
-- already own these objects and baseline this migration during adoption.
--
-- Keep each displayed Username as a temporary, unique key: IX_Users_Username
-- already guarantees uniqueness with the same binary comparison. SQL upper()
-- is deliberately not used because it is ASCII-only. The transactional ICU
-- data backfill replaces these keys before any requests are served.
--
-- This is an additive ALTER, not a table rebuild, so user-dependent foreign
-- keys, permissions, sessions, and watch history remain intact.
ALTER TABLE "Users" ADD COLUMN "NormalizedUsername" TEXT NOT NULL DEFAULT '';
UPDATE "Users" SET "NormalizedUsername" = "Username";
CREATE UNIQUE INDEX "IX_Users_NormalizedUsername" ON "Users" ("NormalizedUsername");
