class CreateIdempotencyKeys < ActiveRecord::Migration[8.1]
  def change
    create_table :idempotency_keys do |t|
      t.string :key, null: false
      t.string :request_path, null: false
      t.text :request_params
      t.text :response_body
      t.integer :response_status
      t.datetime :completed_at

      t.timestamps
    end
    add_index :idempotency_keys, :key, unique: true
    add_index :idempotency_keys, :created_at
  end
end
