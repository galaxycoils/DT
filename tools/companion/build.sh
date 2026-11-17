#!/usr/bin/env sh
set -eu
cd "$(dirname "$0")/../.."
python3 tools/companion/extract-schema.py
npx --yes terser@5.44.1 /tmp/grok-companion-build.js --compress passes=3,toplevel --mangle toplevel --mangle-props regex=/^_/ --output crates/gm-openbot/src/companion_ui.js
node --check crates/gm-openbot/src/companion_ui.js
node -e 'const fs=require("fs");const bytes=["css","js"].reduce((n,e)=>n+fs.statSync("crates/gm-openbot/src/companion_ui."+e).size,0);if(bytes>=45*1024)throw Error("Companion exceeds 45 KiB: "+bytes);console.log("Companion CSS+JS: "+bytes+" bytes");'
