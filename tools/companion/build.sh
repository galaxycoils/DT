#!/usr/bin/env sh
set -eu
cd "$(dirname "$0")/../.."
python3 tools/generate-ui-tokens.py
cat crates/gm-openbot/src/companion_tokens.css crates/gm-openbot/src/companion_layout.css > crates/gm-openbot/src/companion_ui.css
python3 tools/companion/extract-schema.py
node tools/companion/node_modules/terser/bin/terser /tmp/grok-companion-build.js --compress passes=3,toplevel --mangle toplevel --mangle-props regex=/^_/ --output crates/gm-openbot/src/companion_ui.js
node --check crates/gm-openbot/src/companion_ui.js
node -e 'const fs=require("fs");const bytes=["css","js"].reduce((n,e)=>n+fs.statSync("crates/gm-openbot/src/companion_ui."+e).size,0);if(bytes>64*1024)throw new Error("Companion CSS+JS exceeds 64 KiB");console.log("Companion CSS+JS: "+bytes+" bytes");'
