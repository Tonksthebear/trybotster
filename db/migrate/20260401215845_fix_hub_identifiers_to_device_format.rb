# frozen_string_literal: true

# Fix hub identifiers created by CollapseDeviceIntoHub migration.
#
# The migration set identifier = fingerprint (colon format: "aa:bb:cc:..."),
# but the CLI generates identifiers as "device-{hex}" (e.g. "device-aabbcc...").
# This mismatch prevents find_or_initialize_by(identifier:) from finding
# the existing hub, and the fingerprint uniqueness constraint blocks creating
# a new one — leaving all migrated hubs permanently alive=false.
class FixHubIdentifiersToDeviceFormat < ActiveRecord::Migration[8.1]
  def up
    # Convert colon-delimited fingerprint identifiers to device-{hex} format.
    # Only touches hubs whose identifier matches the colon fingerprint pattern.
    execute <<~SQL
      UPDATE hubs
      SET identifier = 'device-' || REPLACE(identifier, ':', '')
      WHERE identifier LIKE '%:%'
    SQL
  end

  def down
    # Convert device-{hex} back to colon-delimited fingerprint format
    execute <<~SQL
      UPDATE hubs
      SET identifier = REGEXP_REPLACE(
        SUBSTRING(identifier FROM 8),
        '(..)', '\\1:', 'g'
      )
      WHERE identifier LIKE 'device-%'
    SQL

    # Trim trailing colon from regexp_replace
    execute <<~SQL
      UPDATE hubs
      SET identifier = LEFT(identifier, LENGTH(identifier) - 1)
      WHERE identifier LIKE '%:'
    SQL
  end
end
