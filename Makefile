# luneta build recipes.
#
# Every recipe needs `nix develop`, or direnv, which enters it for you. The system
# rustc cannot build this crate. See flake.nix.

WASM       := target/wasm32-wasip1/release/luneta.wasm
INSTALL_DIR := $(HOME)/.local/share/zellij/plugins
INSTALLED  := $(INSTALL_DIR)/luneta.wasm
SESSION    ?=

.PHONY: build install reload dev media clean

build:
	cargo build --release --target wasm32-wasip1

# Zellij caches permissions against the absolute path of the .wasm. Keep this path
# stable, or zellij asks for the permissions again after each install.
install: build
	@mkdir -p $(INSTALL_DIR)
	install -m 0644 $(WASM) $(INSTALLED)
	@echo "installed -> $(INSTALLED)"

ZELLIJ_CMD = $(if $(SESSION),zellij -s $(SESSION),zellij)

# Put the new bytes into the picker pane. This needs no zellij restart, asks for no
# permission again, and closes no pane.
#
# Two calls are necessary, because neither one covers both states:
#
#   launch-or-focus  Creates the picker if it does not run, and is the only call
#                    that can pass --floating, which is the geometry the key
#                    binding gives it. On a picker that runs, it takes focus.
#   start-or-reload  Reads the .wasm from disk again and puts it into the pane
#                    that is already there. This is what loads the new bytes.
#
# The order matters. Focus or create first, so that there is something to reload.
#
# Do not return to a close-and-relaunch pair. That version had to name the pane it
# closed, and it could not: zellij documents a `plugin_<id>` on the stdout of
# launch-or-focus-plugin ("Returns: Plugin pane ID"), but 0.45.1 prints nothing.
# The guard of the recipe thus saw an empty id and refused to run, which was
# correct: `close-pane` without an id closes the focused pane, which during
# development is usually your editor. A reload needs no pane id.
#
# Pass SESSION=<name> to drive another session. Omit it to use the current one.
reload:
	$(ZELLIJ_CMD) action launch-or-focus-plugin \
		--skip-plugin-cache --floating file:$(INSTALLED)
	$(ZELLIJ_CMD) action start-or-reload-plugin file:$(INSTALLED)

dev: install reload

# Record the README GIFs with vhs, which drives a real terminal from a script.
# The tapes press real keys against the installed .wasm, so what lands in a GIF is
# the plugin and not a mock. See docs/media/README.md for what they assume.
TAPES := $(wildcard docs/media/*.tape)

media: install
	@command -v vhs >/dev/null || { echo "vhs not found: nix shell nixpkgs#vhs"; exit 1; }
	for tape in $(TAPES); do vhs $$tape; done

clean:
	cargo clean
