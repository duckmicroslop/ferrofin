-- Rust full lowercase can apply contextual/expanding mappings that differ
-- from Jellyfin's invariant simple casing (e.g. ΟΣ -> ος instead of οσ).
-- Only replace keys matching the previous Ferrofin derivation; retain keys
-- from other implementations and custom/imported data.
-- Refresh both sides of the indexed clean-name joins together. IDs, raw names,
-- value uniqueness (Type, Value), and all referencing rows remain unchanged.
-- These application functions are used only during this data migration;
-- no persistent index, trigger, or view depends on a custom SQL function.
UPDATE "BaseItems"
SET "CleanName" = ferrofin_clean_value("Name")
WHERE "Name" IS NOT NULL
  AND "CleanName" = ferrofin_previous_clean_value("Name")
  AND "CleanName" IS NOT ferrofin_clean_value("Name");

UPDATE "ItemValues"
SET "CleanValue" = ferrofin_clean_value("Value")
WHERE "CleanValue" = ferrofin_previous_clean_value("Value")
  AND "CleanValue" IS NOT ferrofin_clean_value("Value");

-- Repair derived sort keys under the same old-value guard. Person names are
-- verbatim unless a forced sort name is set. Preserve the user's raw override
-- and any sort key that does not match our previous derivation.
UPDATE "BaseItems"
SET "SortName" = ferrofin_sort_name("Name")
WHERE "Name" IS NOT NULL
  AND "Type" <> 'MediaBrowser.Controller.Entities.Person'
  AND ("ForcedSortName" IS NULL OR "ForcedSortName" = '')
  AND "SortName" = ferrofin_previous_sort_name("Name")
  AND "SortName" IS NOT ferrofin_sort_name("Name");

UPDATE "BaseItems"
SET "SortName" = ferrofin_forced_sort_key("ForcedSortName")
WHERE "ForcedSortName" IS NOT NULL AND "ForcedSortName" <> ''
  AND "SortName" = ferrofin_previous_forced_sort_key("ForcedSortName")
  AND "SortName" IS NOT ferrofin_forced_sort_key("ForcedSortName");
