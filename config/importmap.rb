# Pin npm packages by running ./bin/importmap

pin "application"
pin "@rails/actioncable", to: "actioncable.esm.js"
pin "@hotwired/turbo-rails", to: "turbo.min.js"
pin "@hotwired/stimulus", to: "stimulus.min.js"
pin "@hotwired/stimulus-loading", to: "stimulus-loading.js"
pin "turbo_stream_update_attribute"
pin "turbo_stream_redirect"
pin_all_from "app/javascript/controllers", under: "controllers"
pin_all_from "app/javascript/channels", under: "channels"
# Restty terminal (libghostty-vt WASM + WebGPU/WebGL2 rendering)

# Vodozemac crypto for E2E encryption (direct Olm)
pin "matrix/bundle", to: "matrix/bundle.js"
pin_all_from "app/javascript/workers", under: "workers"

# Connection management (global, Turbo-aware)
pin "connections", to: "connections/index.js"
pin_all_from "app/javascript/connections", under: "connections"

# Encrypted transport layer for preview
pin_all_from "app/javascript/lib", under: "lib"
pin_all_from "app/javascript/transport", under: "transport"
pin_all_from "app/javascript/preview", under: "preview"
pin_all_from "app/javascript/channels", under: "channels"
pin "@tailwindplus/elements", to: "@tailwindplus--elements.js" # @1.0.22
pin "restty" # @0.1.33
pin "chunk-p8wzkwjt" # @0.1.33
