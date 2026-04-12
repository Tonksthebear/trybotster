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

pin_all_from "app/javascript/lib", under: "lib"
pin_all_from "app/javascript/transport", under: "transport"
pin_all_from "app/javascript/channels", under: "channels"
pin "@tailwindplus/elements", to: "@tailwindplus--elements.js" # @1.0.22
pin "chunk-qj4j7h9k", to: "chunk-qj4j7h9k.js"
pin "restty", to: "restty.js"

# Preact island runtime (htm tagged templates — no build step)
pin "preact", to: "preact.js" # @10.29.1
pin "preact/hooks", to: "preact--hooks.js" # @10.29.1
pin "htm", to: "htm.js" # @3.1.1
pin "htm/preact", to: "htm--preact.js" # @3.1.1
pin "@preact/signals-core", to: "@preact--signals-core.js" # @1.14.1
pin "@preact/signals", to: "@preact--signals.js" # @2.9.0
pin_all_from "app/javascript/island", under: "island"
