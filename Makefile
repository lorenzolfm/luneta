# luneta — the edit -> see-it loop.
#
# All recipes assume you are inside `nix develop` (or have direnv allowed, which
# does it for you). The bare system rustc CANNOT build this: see flake.nix.

WASM       := target/wasm32-wasip1/release/luneta.wasm
INSTALL_DIR := $(HOME)/.local/share/zellij/plugins
INSTALLED  := $(INSTALL_DIR)/luneta.wasm
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

# Put the freshly built bytes into the picker pane. No zellij restart, no
# re-prompt, and — unlike what this used to do — no pane is closed.
#
# Two calls, because neither one covers both states on its own:
#
#   launch-or-focus  the picker may not be running at all. This is the only one
#                    of the two that can create it, and the only one that can
#                    say --floating, which is the geometry the keybinding gives
#                    it. On a picker that IS running it just takes focus.
#   start-or-reload  re-reads the .wasm from disk and swaps it into the pane
#                    that is already there. This is what actually picks up the
#                    new bytes when the pane survived from the last loop.
#
# Order matters: focus-or-create first so that there is something to reload.
# Running it the other way round reloads a picker that may not exist yet and
# then launches one from whatever was already cached.
#
# ⚠️ This replaced a close-and-relaunch pair, and the reason is not tidiness.
# That version had to name the pane it was closing and could not: zellij
# documents a `plugin_<id>` on launch-or-focus-plugin's stdout ("Returns: Plugin
# pane ID") but prints nothing at all on 0.45.1, so the recipe's guard saw an
# empty id and refused to run — correctly, since `close-pane` without an id
# closes whatever is *focused*, which during a dev loop is usually your editor.
# Reloading in place needs no pane id, so there is nothing left to get wrong.
#
# Pass SESSION=<name> to drive another session; omit it to use the current one.
reload:
	$(ZELLIJ_CMD) action launch-or-focus-plugin \
		--skip-plugin-cache --floating file:$(INSTALLED)
	$(ZELLIJ_CMD) action start-or-reload-plugin file:$(INSTALLED)

dev: install reload

clean:
	cargo clean
