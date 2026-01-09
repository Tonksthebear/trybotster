# Pin npm packages by running ./bin/importmap

pin "application"
pin "@rails/actioncable", to: "actioncable.esm.js"
pin "@hotwired/turbo-rails", to: "turbo.min.js"
pin "@hotwired/stimulus", to: "stimulus.min.js"
pin "@hotwired/stimulus-loading", to: "stimulus-loading.js"
pin_all_from "app/javascript/controllers", under: "controllers"
pin_all_from "app/javascript/channels", under: "channels"
# Using esm.sh for proper ESM exports that work with importmaps
pin "@xterm/xterm", to: "https://esm.sh/@xterm/xterm@5.5.0"
pin "@xterm/addon-fit", to: "https://esm.sh/@xterm/addon-fit@0.10.0"

# @noble/* crypto libraries (audited, actively maintained)
pin "@noble/ciphers", to: "https://esm.sh/@noble/ciphers@1.2.1"
pin "@noble/curves", to: "https://esm.sh/@noble/curves@1.8.1"
pin "@noble/hashes", to: "https://esm.sh/@noble/hashes@1.7.1"

# Crypto modules
pin_all_from "app/javascript/crypto", under: "crypto"

# vodozemac WASM wrapper
pin_all_from "app/javascript/wasm", under: "wasm"
