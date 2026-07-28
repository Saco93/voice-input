PREFIX ?= $(HOME)/.local
BIN_DIR ?= $(PREFIX)/bin
SHARE_DIR ?= $(PREFIX)/share/voice-input
QUICKSHELL_DIR ?= $(SHARE_DIR)/quickshell
QUICKSHELL_SETTINGS_DIR ?= $(SHARE_DIR)/quickshell-settings
SYSTEMD_USER_DIR ?= $(HOME)/.config/systemd/user
PI_EXTENSIONS_DIR ?= $(HOME)/.pi/agent/extensions
CONFIG_HOME ?= $(HOME)/.config
CONFIG_DIR ?= $(CONFIG_HOME)/voice-input
CREDENTIAL_STORE_DIR ?= $(CONFIG_HOME)/credstore.encrypted
QSB ?= $(shell command -v qsb 2>/dev/null || command -v qsb6 2>/dev/null || if [ -x /usr/lib/qt6/bin/qsb ]; then printf '%s' /usr/lib/qt6/bin/qsb; fi)
HUD_SHADER_SOURCE := assets/quickshell/shaders/wavy-halo.frag
HUD_SHADER_OUTPUT := target/quickshell/shaders/wavy-halo.frag.qsb

.PHONY: build run install install-hud-assets hud-shaders clean enable-service disable-service

build:
	cargo build --release --offline

run:
	cargo run --offline -- daemon

hud-shaders:
	@test -n "$(QSB)" || { printf '%s\n' 'Qt Shader Tools are required; install qt6-shadertools or set QSB=/path/to/qsb' >&2; exit 1; }
	mkdir -p $(dir $(HUD_SHADER_OUTPUT))
	"$(QSB)" --qt6 -o $(HUD_SHADER_OUTPUT) $(HUD_SHADER_SOURCE)

install-hud-assets: hud-shaders
	install -Dm644 assets/quickshell/shell.qml $(QUICKSHELL_DIR)/shell.qml
	install -Dm644 assets/quickshell/StateStore.qml $(QUICKSHELL_DIR)/StateStore.qml
	install -Dm644 assets/quickshell/HudSurface.qml $(QUICKSHELL_DIR)/HudSurface.qml
	install -Dm644 assets/quickshell/WavyHalo.qml $(QUICKSHELL_DIR)/WavyHalo.qml
	install -Dm644 $(HUD_SHADER_OUTPUT) $(QUICKSHELL_DIR)/shaders/wavy-halo.frag.qsb

install: build install-hud-assets
	install -Dm755 target/release/voice-input $(BIN_DIR)/voice-input
	rm -rf $(QUICKSHELL_SETTINGS_DIR)
	install -d -m755 $(QUICKSHELL_SETTINGS_DIR)
	cp -a assets/quickshell-settings/. $(QUICKSHELL_SETTINGS_DIR)/
	find $(QUICKSHELL_SETTINGS_DIR) -type f -exec chmod 0644 {} +
	# Remove obsolete GTK/Python assets left by upgrades from older releases.
	rm -f $(SHARE_DIR)/hud.py $(SHARE_DIR)/settings.py
	install -Dm644 assets/pi/voice-input-session-registry.ts $(PI_EXTENSIONS_DIR)/voice-input-session-registry.ts
	install -Dm644 assets/config.toml $(SHARE_DIR)/config.toml
	install -Dm644 assets/omarchy-hyprland-snippet.conf $(SHARE_DIR)/omarchy-hyprland-snippet.conf
	install -Dm644 assets/omarchy-waybar-snippet.jsonc $(SHARE_DIR)/omarchy-waybar-snippet.jsonc
	mkdir -p $(SYSTEMD_USER_DIR)
	sed \
		-e 's|@VOICE_INPUT_BIN@|$(BIN_DIR)/voice-input|g' \
		-e 's|@VOICE_INPUT_ASSET_DIR@|$(SHARE_DIR)|g' \
		assets/voice-input.service > $(SYSTEMD_USER_DIR)/voice-input.service
	sed \
		-e 's|@VOICE_INPUT_QUICKSHELL_DIR@|$(QUICKSHELL_DIR)|g' \
		assets/voice-input-hud.service > $(SYSTEMD_USER_DIR)/voice-input-hud.service
	install -d -m700 $(CONFIG_DIR) $(CREDENTIAL_STORE_DIR)
	if [ ! -f $(CONFIG_DIR)/config.toml ]; then \
		if [ -f $(HOME)/.config/voxtype/config.toml ] \
			&& grep -q '^\[llm\]' $(HOME)/.config/voxtype/config.toml \
			&& grep -q '^provider = ' $(HOME)/.config/voxtype/config.toml; then \
			install -m600 $(HOME)/.config/voxtype/config.toml $(CONFIG_DIR)/config.toml; \
		else \
			install -m600 assets/config.toml $(CONFIG_DIR)/config.toml; \
		fi; \
	fi
	for credential in alibaba-api-key openrouter-api-key; do \
		if [ ! -f $(CREDENTIAL_STORE_DIR)/$$credential ]; then \
			for source in \
				$(CONFIG_DIR)/credentials/$$credential.cred \
				$(CONFIG_HOME)/voxtype/credentials/$$credential.cred; do \
				if [ -f $$source ]; then \
					install -m600 $$source $(CREDENTIAL_STORE_DIR)/$$credential; \
					break; \
				fi; \
			done; \
		fi; \
	done
	@printf "Installed voice-input to %s\n" "$(BIN_DIR)/voice-input"
	@printf "Hyprland snippet: %s\n" "$(SHARE_DIR)/omarchy-hyprland-snippet.conf"
	@printf "Waybar snippet:   %s\n" "$(SHARE_DIR)/omarchy-waybar-snippet.jsonc"
	@printf "Quickshell HUD:      %s\n" "$(QUICKSHELL_DIR)"
	@printf "Quickshell Settings: %s\n" "$(QUICKSHELL_SETTINGS_DIR)"
	@printf "Pi session hook:     %s\n" "$(PI_EXTENSIONS_DIR)/voice-input-session-registry.ts"

enable-service: install
	systemctl --user daemon-reload
	systemctl --user enable voice-input.service voice-input-hud.service
	systemctl --user restart voice-input.service voice-input-hud.service

disable-service:
	systemctl --user disable --now voice-input-hud.service voice-input.service || true

clean:
	cargo clean
