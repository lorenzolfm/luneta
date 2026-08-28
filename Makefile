# zj-picker — the edit -> see-it loop.
#
# All recipes assume you are inside `nix develop` (or have direnv allowed, which
# does it for you). The bare system rustc CANNOT build this: see flake.nix.

WASM       := target/wasm32-wasip1/release/zj-picker.wasm
INSTALL_DIR := $(HOME)/.local/share/zellij/plugins
INSTALLED  := $(INSTALL_DIR)/zj-picker.wasm
SESSION    ?=

.PHONY: build install reload dev clean

build:
	cargo build --release --target wasm32-wasip1

# Permissions are cached against the ABSOLUTE path of the .wasm, so keeping the
# installed path stable is what stops zellij re-prompting on every install.
install: build
	@mkdir -p $(INSTALL_DIR)
	install -m 0644 $(WASM) $(INSTALLED)
	@echo "installed -> $(INSTALLED)"

ZELLIJ_CMD = $(if $(SESSION),zellij -s $(SESSION),zellij)

# Close the running plugin pane and relaunch it with the cache skipped, so the
# freshly built bytes are picked up. No zellij restart, no re-prompt.
#
# The first launch-or-focus prints the plugin's pane id (it focuses the existing
# instance if there is one), and we close it BY ID. `close-pane` with no --pane-id
# closes whatever is *focused*, which is not necessarily the plugin.
#
# Pass SESSION=<name> to drive another session; omit it to use the current one.
reload:
	@pid=$$($(ZELLIJ_CMD) action launch-or-focus-plugin --skip-plugin-cache \
		--floating file:$(INSTALLED) | tail -1); \
	case "$$pid" in \
		plugin_*) $(ZELLIJ_CMD) action close-pane --pane-id "$$pid" ;; \
		*) echo "unexpected pane id '$$pid' - refusing to close anything"; exit 1 ;; \
	esac
	$(ZELLIJ_CMD) action launch-or-focus-plugin \
		--skip-plugin-cache --floating file:$(INSTALLED)

dev: install reload

clean:
	cargo clean
