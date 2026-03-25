# Collapse Device Model into Hub Model

**Date:** 2026-03-25
**Status:** Plan (not started)

## Executive Summary

The `Device` model is a vestige from a many-hubs-per-device architecture. Today, the relationship is 1:1 (one CLI device = one hub). "Hub" is the canonical name everywhere in the CLI (Rust/Lua). This refactoring merges Device columns into Hub, renames DeviceToken to HubToken and DeviceAuthorization to HubAuthorization, and extracts browser key registration into its own lightweight model.

**Key insight:** Browser "devices" are NOT devices at all -- they are E2E encryption key registrations. They have no hubs, no tokens, and no device lifecycle. They exist solely so the CLI can look up browser public keys for key exchange. This must be preserved as a separate model.

## Current State Analysis

### Data Model

```
Device (devices table)
  - user_id, device_type (cli/browser), name, public_key,
    fingerprint, notifications_enabled, last_seen_at
  - has_many :hubs
  - has_one :device_token
  - has_one :mcp_token

Hub (hubs table)
  - user_id, device_id (optional FK), identifier, name,
    alive, last_seen_at, message_sequence
  - belongs_to :device, optional: true

DeviceToken (device_tokens table)
  - device_id, token, last_used_at, last_ip

DeviceAuthorization (device_authorizations table)
  - user_code, device_code, device_name, fingerprint,
    status, expires_at, user_id (set on approval)
  - No FK to devices -- standalone OAuth device flow record

MCPToken (integrations_github_mcp_tokens table)
  - device_id, token, last_used_at, last_ip
```

### How Browser Keys Work (Must Not Break)

Browser "devices" are created via `POST /devices` with `device_type: "browser"` and a `public_key`. This is called from the browser's JavaScript crypto layer to register the browser's identity key for E2E key exchange. The flow:

1. Browser generates keypair locally (vodozemac)
2. Browser registers public key via `POST /devices` (session auth)
3. CLI can look up browser keys via `GET /devices` (bearer auth)
4. Key exchange happens via QR code or signaling channel

Browser devices:
- Always have `public_key` (validated)
- Never have a `device_token` or `mcp_token`
- Never have hubs
- Are not shown in Settings > Devices (that page only shows `cli_devices`)
- The `@browser_device` ivar is set in `HubsController#show` and `SessionsController#show` but **never actually used in any view template** -- it appears to be dead code

### What CLI Device Columns Mean on Hub

| Device column | Hub equivalent | Notes |
|---|---|---|
| `device_type` | Always "cli" for hub-linked devices | Drop -- hubs are implicitly CLI |
| `name` | Hub already has `name` | Hub.name falls back to `device.name` today |
| `public_key` | Not stored for CLI in secure mode | Always nil for CLI devices |
| `fingerprint` | Move to Hub | Used for CLI identity/dedup |
| `notifications_enabled` | Move to Hub | Push notification flag |
| `last_seen_at` | Hub already has `last_seen_at` | Redundant |

### Relationship in Practice

- `Hub.belongs_to :device` is optional -- `hub_without_device` fixture proves this
- `HubsController#create` auto-associates the single CLI device if only one exists
- The CLI sends `device_id` in hub registration payload
- Hub.name falls back: `read_attribute(:name) || device&.name || identifier.truncate(20)`

## Identified Issues

### Critical
1. **Device model conflates two unrelated concepts**: CLI identity (auth + hub lifecycle) and browser key registration (E2E crypto)
2. **Naming mismatch**: CLI uses "hub" everywhere; Rails uses "device" for auth tokens and the OAuth flow

### Major
3. **Dead code**: `@browser_device` set in 2 controllers, used in 0 views
4. **Redundant `last_seen_at`**: exists on both Device and Hub
5. **Redundant `name`**: Hub.name has a fallback chain through device.name
6. **Indirection in token auth**: `DeviceToken -> Device -> User` when it could be `HubToken -> Hub -> User`

### Minor
7. **`device_type` column**: Only needed because browser and CLI share a table
8. **`public_key` on Device**: Always nil for CLI devices in secure mode

## Proposed Refactoring Plan

### Phase 0: Extract BrowserKey Model (Pre-requisite, Low Risk)

Separate browser key registration from the Device model before touching anything else.

**New model: `BrowserKey`**
```ruby
# app/models/browser_key.rb
class BrowserKey < ApplicationRecord
  belongs_to :user
  validates :public_key, presence: true, uniqueness: true
  validates :name, presence: true
  validates :fingerprint, presence: true, uniqueness: { scope: :user_id }
  before_validation :generate_fingerprint, on: :create
end
```

**Migration:**
```ruby
create_table :browser_keys do |t|
  t.references :user, null: false, foreign_key: true
  t.string :name, null: false
  t.string :public_key, null: false
  t.string :fingerprint, null: false
  t.datetime :last_seen_at
  t.timestamps
  t.index :public_key, unique: true
  t.index [:user_id, :fingerprint], unique: true
end

# Data migration: copy browser devices to browser_keys
reversible do |dir|
  dir.up do
    execute <<-SQL
      INSERT INTO browser_keys (user_id, name, public_key, fingerprint, last_seen_at, created_at, updated_at)
      SELECT user_id, name, public_key, fingerprint, last_seen_at, created_at, updated_at
      FROM devices WHERE device_type = 'browser'
    SQL
  end
end
```

**Controller changes:**
- `DevicesController` currently handles both types. Split:
  - `POST /devices` with `device_type: "browser"` -> route to `BrowserKeysController` (or keep unified and dispatch internally)
  - `GET /devices` returns both CLI devices and browser keys for key exchange (CLI needs to see browser public keys)
  - Simplest approach: **keep `GET/POST /devices` route but rewrite controller internals** to query from appropriate tables. This avoids breaking the Rust CLI's `POST /devices` and `GET /devices` calls.

**Acceptance criteria:**
- Browser key registration still works via existing API contract
- CLI `GET /devices` still returns browser keys (for key exchange)
- No browser-visible behavior changes
- All existing tests pass

### Phase 1: Merge Device Columns into Hub

Move CLI-device-specific columns onto the Hub table.

**Migration:**
```ruby
# Add device columns to hubs
add_column :hubs, :fingerprint, :string
add_column :hubs, :notifications_enabled, :boolean, default: false, null: false
add_index :hubs, :fingerprint

# Migrate data: copy from device to hub where device_id is set
reversible do |dir|
  dir.up do
    execute <<-SQL
      UPDATE hubs SET
        fingerprint = devices.fingerprint,
        notifications_enabled = devices.notifications_enabled,
        name = COALESCE(NULLIF(hubs.name, ''), devices.name)
      FROM devices
      WHERE hubs.device_id = devices.id
    SQL
  end
end
```

**Model changes:**
- Hub gains: `fingerprint`, `notifications_enabled`
- Hub.name: remove device fallback, use own name directly
- Hub: remove `belongs_to :device` (not yet -- FK still needed for Phase 2)
- Hub: add `e2e_enabled?` based on own state (currently delegates to `device.present?`)

**Acceptance criteria:**
- Hub records have fingerprint and notifications_enabled populated
- Hub.name returns own name (no device fallback)
- Hub.e2e_enabled? works without device association

### Phase 2: DeviceToken -> HubToken

Rename the token model and repoint the FK from device_id to hub_id.

**Migration:**
```ruby
# Rename table
rename_table :device_tokens, :hub_tokens

# Add hub_id column
add_reference :hub_tokens, :hub, foreign_key: true

# Populate hub_id from device_id -> device -> hub
reversible do |dir|
  dir.up do
    execute <<-SQL
      UPDATE hub_tokens SET hub_id = hubs.id
      FROM hubs
      WHERE hub_tokens.device_id = hubs.device_id
    SQL
  end
end

# Make hub_id required, drop device_id
change_column_null :hub_tokens, :hub_id, false
remove_foreign_key :hub_tokens, column: :device_id
remove_column :hub_tokens, :device_id
```

**Model:**
```ruby
# app/models/hub_token.rb (was device_token.rb)
class HubToken < ApplicationRecord
  TOKEN_PREFIX = "btstr_"
  TOKEN_LENGTH = 32
  belongs_to :hub
  encrypts :token, deterministic: true
  validates :token, presence: true, uniqueness: true
  before_validation :generate_token, on: :create

  def user
    hub&.user
  end
end
```

**Affected files:**
- `app/models/hub.rb`: `has_one :hub_token` (was through device)
- `app/models/user.rb`: `has_many :hub_tokens, through: :hubs`
- `app/controllers/concerns/api_key_authenticatable.rb`: `HubToken.find_by(token:)`, then `hub_token.hub&.user`
- `app/channels/application_cable/connection.rb`: same pattern
- `app/controllers/integrations/github/mcp_tokens_controller.rb`: resolve device -> resolve hub from token
- `app/mcp/identifiers/device_token_identifier.rb`: rename to `hub_token_identifier.rb` (or keep class name if ActionMCP requires it)
- `test/support/api_test_helper.rb`: create hub_token directly on hub instead of device
- All test files referencing DeviceToken

**Risk note:** The `btstr_` token prefix does NOT change. Existing tokens in production databases remain valid -- only the table and FK change. No CLI-side changes needed for token format.

### Phase 3: MCPToken FK Change

Repoint MCPToken from device_id to hub_id.

**Migration:**
```ruby
add_reference :integrations_github_mcp_tokens, :hub, foreign_key: true

reversible do |dir|
  dir.up do
    execute <<-SQL
      UPDATE integrations_github_mcp_tokens SET hub_id = hubs.id
      FROM hubs
      WHERE integrations_github_mcp_tokens.device_id = hubs.device_id
    SQL
  end
end

change_column_null :integrations_github_mcp_tokens, :hub_id, false
remove_foreign_key :integrations_github_mcp_tokens, column: :device_id
remove_column :integrations_github_mcp_tokens, :device_id
```

**Model:** `belongs_to :hub` (was `:device`)

**Affected files:**
- `app/models/hub.rb`: `has_one :mcp_token`
- `app/controllers/integrations/github/mcp_tokens_controller.rb`: resolve hub from HubToken, then `@hub.mcp_token`
- `app/controllers/hubs/codes_controller.rb`: token creation -- create tokens on hub instead of device
- `app/mcp/identifiers/device_token_identifier.rb`: MCPToken.user now goes through hub

### Phase 4: DeviceAuthorization -> HubAuthorization

Rename the OAuth device flow model.

**Migration:**
```ruby
rename_table :device_authorizations, :hub_authorizations
```

**Model:** Rename file to `hub_authorization.rb`, class to `HubAuthorization`.

The columns `device_code`, `device_name` can stay as-is -- they are RFC 8628 terminology ("device authorization grant"), not references to the Device model. Alternatively, rename `device_name` to `hub_name` and `device_code` to `authorization_code` for clarity, but this requires CLI changes to the polling endpoint.

**Recommendation:** Keep `device_code` and `device_name` column names. These are OAuth spec terms. Only the table/class name changes.

**Affected files:**
- `app/controllers/hubs/codes_controller.rb`: `HubAuthorization.create!`, find_by, etc.
- `app/controllers/users/hubs_controller.rb`: `HubAuthorization.find_by`
- `app/views/users/hubs/confirm.html.erb`: `@authorization.device_name` (unchanged -- column name stays)

### Phase 5: Drop Device Table and Remove device_id from Hub

Final cleanup after all FKs are repointed.

**Migration:**
```ruby
remove_foreign_key :hubs, :devices
remove_column :hubs, :device_id

# Delete CLI devices (browser devices already migrated to browser_keys in Phase 0)
drop_table :devices
```

**Model cleanup:**
- Delete `app/models/device.rb`
- User: remove `has_many :devices`, remove `has_many :device_tokens, through: :devices`
- Hub: remove `belongs_to :device`

**Controller cleanup:**
- Delete `app/controllers/hubs/device_controller.rb` (the device settings page within a hub)
- Delete `app/views/hubs/device/` directory
- Merge device settings (notifications, fingerprint display) into `hubs/settings/show.html.erb`
- Delete `app/controllers/settings/devices_controller.rb` and `app/views/settings/devices/`
  - Settings > Devices page becomes unnecessary -- hubs list IS the device list now
- Update `DevicesController` to only handle browser keys (or rename to `BrowserKeysController`)

**Route changes:**
```ruby
# Remove
resources :devices  # (or repurpose for browser keys only)
namespace :settings do
  resources :devices  # gone -- hubs list replaces this
end

# Keep/Update
resources :hubs do
  resource :device, ...  # REMOVE -- fold into hub settings
end

# Add (if splitting browser keys to own route)
resources :browser_keys, only: [:index, :create, :destroy]
```

**Sidebar/View cleanup:**
- Remove "Device Settings" link from sidebar (`_sidebar_content.html.erb` line 117)
- Replace `Current.hub.device&.name` references with `Current.hub.name` in views (4 occurrences)
- Remove `hub.device.name` from `_index_hubs.html.erb`
- Remove `includes(:device)` from queries in `ApplicationController`, `HubsController`, `Hub#broadcast_hubs_list`
- Remove dead `@browser_device` assignments from `HubsController#show` and `SessionsController#show`

### Phase 6: CLI (Rust) Updates

The CLI currently has a two-step registration: (1) register device, (2) register hub with device_id.

**After refactoring:**
- `POST /devices` for CLI registration is **no longer needed** -- the hub registration endpoint handles everything
- `POST /hubs` should accept `fingerprint` directly (already accepts `name`)
- `PATCH /devices/:id` for notifications_enabled becomes `PATCH /hubs/:id` with `notifications_enabled`
- `GET /devices` is still needed for browser key exchange -- keep it or rename route

**Rust changes:**
- `cli/src/device.rs`: remove `device_id` field, `register()` method, `set_device_id()`, `clear_device_id()`
  - Keep: keypair generation, fingerprint, signing -- these are still needed
  - Rename `StoredDevice` to something like `Identity` or keep as-is (it is the local config, not the server model)
- `cli/src/hub/registration.rs`:
  - `register_device()` -- delete entirely
  - `register_hub_with_server()` -- send `fingerprint` instead of `device_id`
- `cli/src/hub/server_comms.rs` line ~4846: `set_notifications_enabled` -- PATCH `/hubs/{hub_id}` instead of `/devices/{device_id}`
- `cli/src/hub/mod.rs` line 809: remove `self.register_device()` call from `setup()`
- `cli/src/auth.rs` line 360: token validation via `GET /devices` -- change to a different health-check endpoint

**Important:** The CLI's local `device.json` file is about local identity (signing keys), not the server's Device model. It should keep existing structure but drop `device_id`.

## Risk Assessment

### High Risk

1. **Token migration (Phase 2)**: Existing `btstr_` tokens must remain valid. The data migration must correctly map device_id -> hub_id through the join. **Mitigation:** Run migration in a transaction; verify count of tokens with null hub_id after migration = 0. Devices without hubs will lose their tokens (acceptable -- they are orphaned).

2. **CLI backward compatibility (Phase 6)**: The CLI sends `device_id` in hub registration. If we deploy the Rails changes before the CLI update, the `device_id` param becomes a no-op (ignored). **Mitigation:** Make Rails ignore unknown `device_id` param gracefully first (it already does -- `find_by` returns nil, auto-association skipped). Deploy Rails first, then CLI.

### Medium Risk

3. **Browser key exchange**: `GET /devices` must continue returning browser public keys for CLI key exchange. **Mitigation:** Phase 0 handles this first, before any destructive changes. The endpoint can query BrowserKey instead of Device.

4. **ActionMCP identifier**: `DeviceTokenIdentifier` resolves MCPToken -> user. The resolution chain changes (MCPToken -> hub -> user instead of MCPToken -> device -> user). **Mitigation:** Simple, testable change.

5. **Codes controller token creation**: `create_device_tokens()` in `CodesController` currently creates a Device, DeviceToken, and MCPToken after authorization approval. This becomes: create Hub (or find existing by fingerprint), create HubToken and MCPToken. **Mitigation:** This is the most complex logic change -- needs careful test coverage.

### Low Risk

6. **View changes**: Replacing `device.name` with `hub.name` and removing device settings pages. Straightforward find-and-replace.

7. **Fixture updates**: Test fixtures need updating but this is mechanical.

## Testing Strategy

### Before Starting
- Snapshot current test suite pass rate (baseline)
- Identify all tests touching Device, DeviceToken, DeviceAuthorization

### Per Phase
- **Phase 0:** Add BrowserKey model tests. Verify `POST /devices` with `device_type: "browser"` creates BrowserKey. Verify `GET /devices` returns browser keys.
- **Phase 1:** Verify Hub now carries fingerprint/notifications_enabled. Verify Hub.name works without device.
- **Phase 2:** Verify token auth still works end-to-end (ApiKeyAuthenticatable, ActionCable connection, MCP identifier). Key test: create hub, create token, authenticate, assert user resolved.
- **Phase 3:** Verify MCP token creation and resolution.
- **Phase 4:** Verify device authorization (OAuth) flow end-to-end: code generation, polling, approval, token issuance.
- **Phase 5:** Full regression. No references to Device model remain.
- **Phase 6:** Rust `test.sh` passes. Hub registration works without device_id. Notifications PATCH works against hub endpoint.

### Integration Tests to Add
- CLI pairing flow (codes -> approve -> hub registered with tokens)
- Browser key exchange (register browser key, CLI fetches it)
- Push notification enable/disable through hub endpoint

## Success Metrics

1. `Device` model and `devices` table deleted
2. `DeviceToken` renamed to `HubToken`, FK points to `hubs`
3. `DeviceAuthorization` renamed to `HubAuthorization`
4. `MCPToken` FK points to `hubs`
5. Browser key registration preserved via `BrowserKey` model
6. CLI registers hub without separate device registration step
7. All existing tests pass (adjusted for new names)
8. No `device_id` column on `hubs` table
9. Token format (`btstr_`, `btmcp_`) unchanged -- zero CLI token rotation needed

## Execution Order

```
Phase 0 (BrowserKey extraction)     -- deploy to prod, verify
Phase 1 (merge columns into Hub)    -- deploy, verify
Phase 2 (DeviceToken -> HubToken)   -- deploy, verify auth works
Phase 3 (MCPToken FK change)        -- deploy, verify MCP works
Phase 4 (DeviceAuthorization rename) -- deploy, verify OAuth flow
Phase 5 (drop Device table)         -- deploy, final cleanup
Phase 6 (CLI updates)               -- release new CLI binary
```

Phases 2-4 can potentially be combined into a single deploy if the migration is tested thoroughly. Phase 6 (CLI) can happen in parallel with Phases 1-5 since the CLI changes are backward-compatible (the server can ignore device_id gracefully).

## Files Affected (Complete List)

### Models
- `app/models/device.rb` -- DELETE (Phase 5)
- `app/models/device_token.rb` -- RENAME to `hub_token.rb` (Phase 2)
- `app/models/device_authorization.rb` -- RENAME to `hub_authorization.rb` (Phase 4)
- `app/models/hub.rb` -- ADD columns, REMOVE device association (Phase 1, 5)
- `app/models/user.rb` -- UPDATE associations (Phase 2, 5)
- `app/models/integrations/github/mcp_token.rb` -- CHANGE FK (Phase 3)
- NEW: `app/models/browser_key.rb` (Phase 0)

### Controllers
- `app/controllers/devices_controller.rb` -- REWRITE for browser keys only (Phase 0, 5)
- `app/controllers/hubs_controller.rb` -- REMOVE device logic (Phase 1, 5)
- `app/controllers/hubs/device_controller.rb` -- DELETE (Phase 5)
- `app/controllers/hubs/codes_controller.rb` -- REWRITE token creation (Phase 2, 3)
- `app/controllers/hubs/sessions_controller.rb` -- REMOVE dead `@browser_device` (Phase 5)
- `app/controllers/hubs/webrtc_controller.rb` -- UPDATE auth resolution (Phase 2)
- `app/controllers/settings/devices_controller.rb` -- DELETE (Phase 5)
- `app/controllers/users/hubs_controller.rb` -- CLASS RENAME only (Phase 4)
- `app/controllers/concerns/api_key_authenticatable.rb` -- HubToken (Phase 2)
- `app/controllers/application_controller.rb` -- REMOVE `includes(:device)` (Phase 5)
- `app/controllers/integrations/github/mcp_tokens_controller.rb` -- CHANGE resolution (Phase 2, 3)

### MCP
- `app/mcp/identifiers/device_token_identifier.rb` -- UPDATE resolution chain (Phase 3)

### Channels
- `app/channels/application_cable/connection.rb` -- HubToken (Phase 2)

### Views (Phase 5)
- `app/views/hubs/show.html.erb` -- replace `device.name` refs
- `app/views/hubs/sessions/show.html.erb` -- replace `device.name` refs
- `app/views/hubs/settings/show.html.erb` -- replace `device.name` refs
- `app/views/hubs/device/` -- DELETE directory
- `app/views/hubs/device/_spawn_target_browser.html.erb` -- MOVE to hub settings
- `app/views/hubs/_index_hubs.html.erb` -- remove device.name
- `app/views/layouts/_sidebar_content.html.erb` -- remove device settings link
- `app/views/settings/devices/` -- DELETE directory
- `app/views/settings/show.html.erb` -- remove devices link

### Routes
- `config/routes.rb` -- remove device routes, add browser_keys

### Tests (25 files reference Device)
- `test/fixtures/devices.yml` -- SPLIT: browser -> browser_keys.yml
- `test/fixtures/device_tokens.yml` -- RENAME to hub_tokens.yml
- `test/fixtures/hubs.yml` -- remove device references
- `test/support/api_test_helper.rb` -- create HubToken on Hub directly
- All 25 test files listed in analysis -- update references

### Rust CLI (Phase 6)
- `cli/src/device.rs` -- remove `device_id`, `register()`, `set_device_id()`, `clear_device_id()`
- `cli/src/hub/registration.rs` -- remove `register_device()`, send fingerprint in hub registration
- `cli/src/hub/server_comms.rs` -- notifications PATCH to hub endpoint
- `cli/src/hub/mod.rs` -- remove `register_device()` call from setup
- `cli/src/auth.rs` -- change token validation endpoint
