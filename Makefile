# Convenience targets. Tauri CLI resolves the project from crates/craft-status-app.
APP_DIR    := crates/craft-status-app
APP_NAME   := Craft Status
BUNDLE     := target/release/bundle/macos/$(APP_NAME).app
INSTALLED  := /Applications/$(APP_NAME).app
AGENT_ID   := ai.storyteller.craft-status
AGENT      := $(HOME)/Library/LaunchAgents/$(AGENT_ID).plist
# Linux install paths (XDG)
BIN        := $(HOME)/.local/bin/craft-status-app
ICON       := $(HOME)/.local/share/icons/hicolor/128x128/apps/$(AGENT_ID).png
DESKTOP    := $(HOME)/.local/share/applications/$(AGENT_ID).desktop
AUTOSTART  := $(HOME)/.config/autostart/$(AGENT_ID).desktop

.PHONY: dev run build bundle install uninstall test check ui snapshot dump preview icons clean

dev: node_modules ## tauri dev, recompiling ui/src/ on change (live-reloads ui/)
	npm run --silent watch & trap 'kill $$!' EXIT; cd $(APP_DIR) && cargo tauri dev

run: node_modules ## fast debug binary, no bundling
	cargo run -p craft-status-app

build: node_modules ## release build + every installer for this OS
	cd $(APP_DIR) && cargo tauri build

bundle: node_modules ## release .app only (macOS)
	cd $(APP_DIR) && cargo tauri build --bundles app

ifeq ($(shell uname -s),Darwin)
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

else
# Linux truncates process names to 15 chars, hence craft-status-ap.
install: node_modules ## build, install to ~/.local, start at login, launch now (Linux)
	cd $(APP_DIR) && cargo tauri build --no-bundle
	-pkill -x craft-status-ap; sleep 1
	install -Dm755 target/release/craft-status-app "$(BIN)"
	install -Dm644 $(APP_DIR)/icons/128x128.png "$(ICON)"
	mkdir -p "$(dir $(DESKTOP))" "$(dir $(AUTOSTART))"
	printf '%s\n' '[Desktop Entry]' 'Type=Application' 'Name=$(APP_NAME)' \
	  'Comment=Status of the storytold craft repositories' \
	  'Exec=$(BIN)' 'Icon=$(AGENT_ID)' 'Categories=Development;' 'Terminal=false' > "$(DESKTOP)"
	cp "$(DESKTOP)" "$(AUTOSTART)"
	setsid -f "$(BIN)" >/dev/null 2>&1

uninstall:      ## quit, remove the app and the login item (keeps config + cache)
	-pkill -x craft-status-ap
	rm -f "$(BIN)" "$(ICON)" "$(DESKTOP)" "$(AUTOSTART)"
endif

test: node_modules ## Rust + UI tests
	cargo test --workspace
	npm test

check: node_modules ## lints
	cargo clippy --workspace --all-targets -- -D warnings
	cargo fmt --all -- --check
	npm run --silent typecheck

snapshot:       ## fetch once from GitHub into ui/dev-snapshot.json (prints a table)
	cargo run -q -p craft-github --bin craft-fetch -- --snapshot ui/dev-snapshot.json

dump:           ## write the cached PR views to ui/dev-data.json, fetching nothing
	cargo run -q -p pr-github --bin pr-fetch -- --dump ui/dev-data.json

preview: snapshot dump ui ## open the UI in a browser against the dev data
	@echo "http://127.0.0.1:8766/index.html  (Ctrl+C to stop)"
	python3 -m http.server 8766 --bind 127.0.0.1 --directory ui

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
