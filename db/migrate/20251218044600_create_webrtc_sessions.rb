class CreateWebrtcSessions < ActiveRecord::Migration[8.1]
  def change
    create_table :webrtc_sessions do |t|
      t.references :user, null: false, foreign_key: true
      t.jsonb :offer, null: false
      t.jsonb :answer
      t.string :status, null: false, default: "pending"
      t.datetime :expires_at, null: false

      t.timestamps
    end

    add_index :webrtc_sessions, :status
    add_index :webrtc_sessions, :expires_at
  end
end
