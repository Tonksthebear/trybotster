# Be sure to restart your server when you modify this file.

# Version of your assets, change this if you want to expire all your assets.
Rails.application.config.assets.version = "1.0"

# Add additional assets to the asset load path.
# Rails.application.config.assets.paths << Emoji.images_path

# Add WASM files to asset pipeline
Rails.application.config.assets.paths << Rails.root.join("app/assets/wasm")

# Add Web Workers to asset pipeline
Rails.application.config.assets.paths << Rails.root.join("app/javascript/workers")

# Register WASM MIME type
Mime::Type.register "application/wasm", :wasm
