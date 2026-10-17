# Convenience targets. Tauri CLI resolves the project from crates/craft-status-app.
APP_DIR    := crates/craft-status-app
APP_NAME   := Craft Status
BUNDLE     := target/release/bundle/macos/$(APP_NAME).app
INSTALLED  := /Applications/$(APP_NAME).app
AGENT_ID   := ai.storyteller.craft-status
AGENT      := $(HOME)/Library/LaunchAgents/$(AGENT_ID).plist

.PHONY: dev run build bundle install uninstall test check ui snapshot preview web icons clean

dev: node_modules ## tauri dev, recompiling ui/src/ on change (live-reloads ui/)
	npm run --silent watch & trap 'kill $$!' EXIT; cd $(APP_DIR) && cargo tauri dev

run: node_modules ## fast debug binary, no bundling
	cargo run -p craft-status-app

build: node_modules ## release build + every installer for this OS
	cd $(APP_DIR) && cargo tauri build

bundle: node_modules ## release .app only (macOS)
	cd $(APP_DIR) && cargo tauri build --bundles app

install: bundle ## copy to /Applications, start at login, launch now (macOS)
	-pkill -x craft-status-app; sleep 1
	rm -rf "$(INSTALLED)" && cp -R "$(BUNDLE)" "$(INSTALLED)"
	mkdir -p "$(HOME)/Library/LaunchAgents"
	printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>' \
	  '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
	  '<plist version="1.0"><dict>' \
	  '<key>Label</key><string>$(AGENT_ID)</string>' \
	  '<key>ProgramArguments</key><array><string>/usr/bin/open</string><string>-a</string><string>$(INSTALLED)</string></array>' \
	  '<key>RunAtLoad</key><true/>' \
	  '</dict></plist>' > "$(AGENT)"
	open "$(INSTALLED)"

uninstall:      ## quit, remove the app and the login item (keeps config + cache)
	-pkill -x craft-status-app
	rm -rf "$(INSTALLED)" "$(AGENT)"

test: node_modules ## Rust + UI tests
	cargo test --workspace
	npm test

check: node_modules ## lints
	cargo clippy --workspace --all-targets -- -D warnings
	cargo fmt --all -- --check
	npm run --silent typecheck

snapshot:       ## fetch once from GitHub into ui/dev-snapshot.json (prints a table)
	cargo run -q -p craft-github --bin craft-fetch -- --snapshot ui/dev-snapshot.json

preview: snapshot ui ## open the UI in a browser against the dev snapshot
	@echo "http://127.0.0.1:8766/index.html  (Ctrl+C to stop)"
	python3 -m http.server 8766 --bind 127.0.0.1 --directory ui

web: snapshot ui ## package static web assets into _site/
	mkdir -p _site
	cp ui/index.html ui/style.css ui/dev-snapshot.json _site/
	cp -R ui/build _site/build

icons:          ## regenerate icons from the SVG sources
	cd $(APP_DIR) && cargo tauri icon icons-src/app-icon.svg -o icons && rm -rf icons/android icons/ios
	cd $(APP_DIR) && cargo tauri icon icons-src/tray.svg -o /tmp/craft-tray -p 64 && cp /tmp/craft-tray/64x64.png icons/tray.png

clean:
	cargo clean

node_modules: package.json package-lock.json
	npm ci
	@touch node_modules

ui: node_modules ## compile ui/src/*.ts to ui/build/ (build.rs does this for app builds)
	npm run --silent build
