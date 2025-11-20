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

ActiveRecord::Schema[8.1].define(version: 2025_11_20_010348) do
  # These are extensions that must be enabled in order to support this database
  enable_extension "pg_catalog.plpgsql"
  enable_extension "vector"

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

  create_table "bot_messages", force: :cascade do |t|
    t.datetime "acknowledged_at"
    t.datetime "claimed_at"
    t.bigint "claimed_by_user_id"
    t.datetime "created_at", null: false
    t.string "event_type", null: false
    t.jsonb "payload", default: {}, null: false
    t.datetime "sent_at"
    t.string "status", default: "pending", null: false
    t.datetime "updated_at", null: false
    t.index ["acknowledged_at"], name: "index_bot_messages_on_acknowledged_at"
    t.index ["claimed_at"], name: "index_bot_messages_on_claimed_at"
    t.index ["claimed_by_user_id"], name: "index_bot_messages_on_claimed_by_user_id"
    t.index ["event_type"], name: "index_bot_messages_on_event_type"
    t.index ["sent_at"], name: "index_bot_messages_on_sent_at"
    t.index ["status"], name: "index_bot_messages_on_status"
  end

  create_table "memories", force: :cascade do |t|
    t.text "content", null: false
    t.datetime "created_at", null: false
    t.vector "embedding", limit: 1536
    t.string "memory_type", default: "other"
    t.jsonb "metadata", default: {}
    t.bigint "parent_id"
    t.string "source"
    t.bigint "team_id"
    t.datetime "updated_at", null: false
    t.bigint "user_id", null: false
    t.string "visibility", default: "private", null: false
    t.index ["created_at"], name: "index_memories_on_created_at"
    t.index ["memory_type"], name: "index_memories_on_memory_type"
    t.index ["metadata"], name: "index_memories_on_metadata", using: :gin
    t.index ["parent_id"], name: "index_memories_on_parent_id"
    t.index ["source"], name: "index_memories_on_source"
    t.index ["team_id"], name: "index_memories_on_team_id"
    t.index ["user_id"], name: "index_memories_on_user_id"
    t.index ["visibility"], name: "index_memories_on_visibility"
  end

  create_table "memory_tags", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.bigint "memory_id", null: false
    t.bigint "tag_id", null: false
    t.datetime "updated_at", null: false
    t.index ["memory_id", "tag_id"], name: "index_memory_tags_on_memory_id_and_tag_id", unique: true
    t.index ["memory_id"], name: "index_memory_tags_on_memory_id"
    t.index ["tag_id"], name: "index_memory_tags_on_tag_id"
  end

  create_table "solid_mcp_messages", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.text "data"
    t.datetime "delivered_at"
    t.string "event_type", limit: 50, null: false
    t.string "session_id", limit: 36, null: false
    t.index ["delivered_at", "created_at"], name: "idx_solid_mcp_messages_on_delivered_and_created"
    t.index ["session_id", "id"], name: "idx_solid_mcp_messages_on_session_and_id"
  end

  create_table "tags", force: :cascade do |t|
    t.datetime "created_at", null: false
    t.string "name"
    t.datetime "updated_at", null: false
    t.index ["name"], name: "index_tags_on_name", unique: true
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
  add_foreign_key "memories", "memories", column: "parent_id"
  add_foreign_key "memories", "teams"
  add_foreign_key "memories", "users"
  add_foreign_key "memory_tags", "memories"
  add_foreign_key "memory_tags", "tags"
  add_foreign_key "users", "teams"
end
