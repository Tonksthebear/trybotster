# This file is auto-generated from the current state of the database. Instead
# of editing this file, please use the migrations feature of Active Record to
# incrementally modify your database, and then regenerate this schema definition.
#
# This file is the source Rails uses to define your schema when running `bin/rails
# db:schema:load`. When creating a new database, `bin/rails db:schema:load` tends to
# be faster and is potentially less error prone than running all of your
# migrations from scratch. Old migrations may fail to apply correctly if those
# migrations use external dependencies or application code.
#
# It's strongly recommended that you check this file into your version control system.

ActiveRecord::Schema[8.1].define(version: 2026_02_19_065753) do
  # These are extensions that must be enabled in order to support this database
  enable_extension "pg_catalog.plpgsql"

  create_table "action_mcp_session_messages", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.string "direction", default: "client", null: false, comment: "The message recipient"
    t.boolean "is_ping", default: false, null: false, comment: "Whether the message is a ping"
    t.string "jsonrpc_id"
    t.json "message_json"
    t.string "message_type", null: false, comment: "The type of the message"
    t.boolean "request_acknowledged", default: false, null: false
    t.boolean "request_cancelled", default: false, null: false
    t.string "session_id", null: false
    t.datetime "updated_at", null: false
    t.index ["session_id"], name: "index_action_mcp_session_messages_on_session_id"
  end

  create_table "action_mcp_session_resources", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.boolean "created_by_tool", default: false
    t.text "description"
    t.datetime "last_accessed_at"
    t.json "metadata"
    t.string "mime_type", null: false
    t.string "name"
    t.string "session_id", null: false
    t.datetime "updated_at", null: false
    t.string "uri", null: false
    t.index ["session_id"], name: "index_action_mcp_session_resources_on_session_id"
  end

  create_table "action_mcp_session_subscriptions", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.datetime "last_notification_at"
    t.string "session_id", null: false
    t.datetime "updated_at", null: false
    t.string "uri", null: false
    t.index ["session_id"], name: "index_action_mcp_session_subscriptions_on_session_id"
  end

  create_table "action_mcp_sessions", id: :string, force: :cascade do |t|
    t.json "client_capabilities", comment: "The capabilities of the client"
    t.json "client_info", comment: "The information about the client"
    t.json "consents", default: {}, null: false
    t.datetime "created_at", null: false
    t.datetime "ended_at", comment: "The time the session ended"
    t.boolean "initialized", default: false, null: false
    t.integer "messages_count", default: 0, null: false
    t.json "prompt_registry", default: []
    t.string "protocol_version"
    t.json "resource_registry", default: []
    t.string "role", default: "server", null: false, comment: "The role of the session"
    t.json "server_capabilities", comment: "The capabilities of the server"
    t.json "server_info", comment: "The information about the server"
    t.integer "sse_event_counter", default: 0, null: false
    t.string "status", default: "pre_initialize", null: false
    t.json "tool_registry", default: []
    t.datetime "updated_at", null: false
  end

  create_table "action_mcp_sse_events", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.text "data", null: false
    t.integer "event_id", null: false
    t.string "session_id", null: false
    t.datetime "updated_at", null: false
    t.index ["created_at"], name: "index_action_mcp_sse_events_on_created_at"
    t.index ["session_id", "event_id"], name: "index_action_mcp_sse_events_on_session_id_and_event_id", unique: true
    t.index ["session_id"], name: "index_action_mcp_sse_events_on_session_id"
  end

  create_table "device_authorizations", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.string "device_code", null: false
    t.string "device_name"
    t.datetime "expires_at", null: false
    t.string "fingerprint"
    t.string "status", default: "pending", null: false
    t.datetime "updated_at", null: false
    t.string "user_code", null: false
    t.bigint "user_id"
    t.index ["device_code"], name: "index_device_authorizations_on_device_code", unique: true
    t.index ["user_code"], name: "index_device_authorizations_on_user_code", unique: true
    t.index ["user_id"], name: "index_device_authorizations_on_user_id"
  end

  create_table "device_tokens", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.bigint "device_id", null: false
    t.string "last_ip"
    t.datetime "last_used_at"
    t.string "name"
    t.string "token", null: false
    t.datetime "updated_at", null: false
    t.index ["device_id"], name: "index_device_tokens_on_device_id"
    t.index ["token"], name: "index_device_tokens_on_token", unique: true
  end

  create_table "devices", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.string "device_type", null: false
    t.string "fingerprint", null: false
    t.datetime "last_seen_at"
    t.string "name", null: false
    t.boolean "notifications_enabled", default: false, null: false
    t.string "public_key"
    t.datetime "updated_at", null: false
    t.bigint "user_id", null: false
    t.index ["fingerprint"], name: "index_devices_on_fingerprint"
    t.index ["public_key"], name: "index_devices_on_public_key", unique: true, where: "(public_key IS NOT NULL)"
    t.index ["user_id", "device_type"], name: "index_devices_on_user_id_and_device_type"
    t.index ["user_id"], name: "index_devices_on_user_id"
  end

  create_table "github_messages", force: :cascade do |t|
    t.datetime "acknowledged_at"
    t.datetime "created_at", null: false
    t.string "event_type", null: false
    t.integer "issue_number"
    t.jsonb "payload", default: {}, null: false
    t.string "repo", null: false
    t.string "status", default: "pending", null: false
    t.datetime "updated_at", null: false
    t.index ["event_type"], name: "index_github_messages_on_event_type"
    t.index ["repo", "status"], name: "index_github_messages_on_repo_and_status"
    t.index ["repo"], name: "index_github_messages_on_repo"
    t.index ["status"], name: "index_github_messages_on_status"
  end

  create_table "hub_agents", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.bigint "hub_id", null: false
    t.string "last_invocation_url"
    t.string "session_key", null: false
    t.datetime "tunnel_connected_at"
    t.datetime "tunnel_last_request_at"
    t.integer "tunnel_port"
    t.string "tunnel_status", default: "disconnected"
    t.datetime "updated_at", null: false
    t.index ["hub_id", "session_key"], name: "index_hub_agents_on_hub_id_and_session_key", unique: true
    t.index ["hub_id"], name: "index_hub_agents_on_hub_id"
  end

  create_table "hub_commands", force: :cascade do |t|
    t.datetime "acknowledged_at"
    t.datetime "created_at", null: false
    t.string "event_type", null: false
    t.bigint "hub_id", null: false
    t.jsonb "payload", default: {}, null: false
    t.bigint "sequence", null: false
    t.string "status", default: "pending", null: false
    t.datetime "updated_at", null: false
    t.index ["hub_id", "sequence"], name: "index_hub_commands_on_hub_id_and_sequence", unique: true
    t.index ["hub_id"], name: "index_hub_commands_on_hub_id"
    t.index ["status"], name: "index_hub_commands_on_status"
  end

  create_table "hubs", force: :cascade do |t|
    t.boolean "alive", default: false, null: false
    t.datetime "created_at", null: false
    t.bigint "device_id"
    t.string "identifier", null: false
    t.datetime "last_seen_at", null: false
    t.bigint "message_sequence", default: 0, null: false
    t.string "name"
    t.datetime "updated_at", null: false
    t.bigint "user_id", null: false
    t.index ["device_id"], name: "index_hubs_on_device_id"
    t.index ["identifier"], name: "index_hubs_on_identifier", unique: true
    t.index ["user_id"], name: "index_hubs_on_user_id"
  end

  create_table "idempotency_keys", force: :cascade do |t|
    t.datetime "completed_at"
    t.datetime "created_at", null: false
    t.string "key", null: false
    t.text "request_params"
    t.string "request_path", null: false
    t.text "response_body"
    t.integer "response_status"
    t.datetime "updated_at", null: false
    t.index ["created_at"], name: "index_idempotency_keys_on_created_at"
    t.index ["key"], name: "index_idempotency_keys_on_key", unique: true
  end

  create_table "integrations_github_mcp_tokens", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.bigint "device_id", null: false
    t.string "last_ip"
    t.datetime "last_used_at"
    t.string "name"
    t.string "token"
    t.datetime "updated_at", null: false
    t.index ["device_id"], name: "index_integrations_github_mcp_tokens_on_device_id"
    t.index ["token"], name: "index_integrations_github_mcp_tokens_on_token", unique: true
  end

  create_table "teams", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.string "name"
    t.datetime "updated_at", null: false
  end

  create_table "users", force: :cascade do |t|
    t.string "api_key"
    t.datetime "created_at", null: false
    t.datetime "current_sign_in_at"
    t.string "current_sign_in_ip"
    t.string "email", default: "", null: false
    t.jsonb "github_app_permissions", default: {}
    t.string "github_app_refresh_token"
    t.string "github_app_token"
    t.datetime "github_app_token_expires_at"
    t.datetime "last_sign_in_at"
    t.string "last_sign_in_ip"
    t.string "provider"
    t.datetime "remember_created_at"
    t.string "remember_token"
    t.integer "sign_in_count", default: 0, null: false
    t.bigint "team_id"
    t.string "uid"
    t.datetime "updated_at", null: false
    t.string "username"
    t.index ["api_key"], name: "index_users_on_api_key", unique: true
    t.index ["email"], name: "index_users_on_email", unique: true
    t.index ["github_app_token_expires_at"], name: "index_users_on_github_app_token_expires_at"
    t.index ["provider", "uid"], name: "index_users_on_provider_and_uid", unique: true
    t.index ["team_id"], name: "index_users_on_team_id"
  end

  add_foreign_key "action_mcp_session_messages", "action_mcp_sessions", column: "session_id", name: "fk_action_mcp_session_messages_session_id", on_update: :cascade, on_delete: :cascade
  add_foreign_key "action_mcp_session_resources", "action_mcp_sessions", column: "session_id", on_delete: :cascade
  add_foreign_key "action_mcp_session_subscriptions", "action_mcp_sessions", column: "session_id", on_delete: :cascade
  add_foreign_key "action_mcp_sse_events", "action_mcp_sessions", column: "session_id"
  add_foreign_key "device_authorizations", "users"
  add_foreign_key "device_tokens", "devices"
  add_foreign_key "devices", "users"
  add_foreign_key "hub_agents", "hubs"
  add_foreign_key "hub_commands", "hubs"
  add_foreign_key "hubs", "devices"
  add_foreign_key "hubs", "users"
  add_foreign_key "integrations_github_mcp_tokens", "devices"
  add_foreign_key "users", "teams"
end
