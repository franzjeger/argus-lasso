PREFIX    ?= $(HOME)/.local
BINDIR     = $(PREFIX)/bin
ICONBASE   = $(PREFIX)/share/icons/hicolor
DESKTOPDIR = $(PREFIX)/share/applications
SYSTEMDDIR = $(HOME)/.config/systemd/user

CARGO_TARGET_DIR ?= target
BINARY     ?= $(CARGO_TARGET_DIR)/release/argus-lasso

# Icon masters, tiered by detail. Downscaling one big raster turned the chip
# pins and the 3x3 core grid into mush below ~48px, so each size range renders
# from a master drawn for it: full art at 64+, no pins at 48, and just the
# lasso ring plus a 2x2 core block at 32 and under.
ICON_SVG        = assets/icon.svg
ICON_SVG_MEDIUM = assets/icon-medium.svg
ICON_SVG_SMALL  = assets/icon-small.svg
ICON_PNG        = assets/icon.png
ICON_PNG_SIZE   = 256

.PHONY: build install reinstall uninstall enable disable install-icons icon

build:
	cargo build --release

# Regenerate the raster that gets embedded in the binary (window + tray icon).
# build.rs reads its dimensions off the PNG, so ICON_PNG_SIZE is free to change.
icon:
	rsvg-convert -w $(ICON_PNG_SIZE) -h $(ICON_PNG_SIZE) $(ICON_SVG) -o $(ICON_PNG)
	@echo "Regenerated $(ICON_PNG) at $(ICON_PNG_SIZE)x$(ICON_PNG_SIZE)."

install-icons:
	@echo "Rendering icons from vector masters…"
	@for size in 16 22 24 32 48 64 128 256; do \
		case $$size in \
			16|22|24|32) src=$(ICON_SVG_SMALL) ;; \
			48)          src=$(ICON_SVG_MEDIUM) ;; \
			*)           src=$(ICON_SVG) ;; \
		esac; \
		dir=$(ICONBASE)/$${size}x$${size}/apps; \
		mkdir -p "$$dir"; \
		rsvg-convert -w $$size -h $$size "$$src" -o "$$dir/argus-lasso.png" 2>/dev/null \
		  || magick -background none "$$src" -resize $${size}x$${size} "$$dir/argus-lasso.png" 2>/dev/null \
		  || magick $(ICON_PNG) -resize $${size}x$${size} "$$dir/argus-lasso.png" 2>/dev/null \
		  || cp $(ICON_PNG) "$$dir/argus-lasso.png"; \
	done
	@echo "Installing scalable master…"
	@mkdir -p $(ICONBASE)/scalable/apps
	@cp $(ICON_SVG) $(ICONBASE)/scalable/apps/argus-lasso.svg

install: build install-icons
	@echo "Installing binary…"
	install -Dm755 $(BINARY) $(BINDIR)/argus-lasso
	@echo "Installing .desktop entry…"
	sed 's|^Exec=argus-lasso|Exec=$(BINDIR)/argus-lasso|' dist/argus-lasso.desktop > $(DESKTOPDIR)/argus-lasso.desktop
	chmod 644 $(DESKTOPDIR)/argus-lasso.desktop
	@echo "Installing systemd user service…"
	install -Dm644 dist/argus-lasso.service $(SYSTEMDDIR)/argus-lasso.service
	systemctl --user daemon-reload
	@echo "Refreshing icon and desktop caches…"
	-update-desktop-database $(DESKTOPDIR)
	-gtk-update-icon-cache -f -t $(PREFIX)/share/icons/hicolor/
	-kbuildsycoca6 --noincremental 2>/dev/null || kbuildsycoca5 --noincremental 2>/dev/null || true
	@echo "Done. Run 'make enable' to autostart on login."

reinstall: build
	@echo "Installing binary…"
	install -Dm755 $(BINARY) $(BINDIR)/argus-lasso
	@echo "Restarting argus-lasso…"
	@if systemctl --user is-active --quiet argus-lasso.service; then \
		systemctl --user restart argus-lasso.service; \
		echo "Restarted via systemd."; \
	else \
		pkill -x argus-lasso 2>/dev/null || true; \
		nohup $(BINDIR)/argus-lasso &>/dev/null & \
		echo "Restarted as background process."; \
	fi

uninstall:
	rm -f $(BINDIR)/argus-lasso
	find $(ICONBASE) -name "argus-lasso.png" -delete 2>/dev/null || true
	rm -f $(ICONBASE)/scalable/apps/argus-lasso.svg
	rm -f $(DESKTOPDIR)/argus-lasso.desktop
	systemctl --user disable --now argus-lasso.service 2>/dev/null || true
	rm -f $(SYSTEMDDIR)/argus-lasso.service
	systemctl --user daemon-reload
	@echo "Uninstalled."

enable:
	systemctl --user enable --now argus-lasso.service
	@echo "argus-lasso will start automatically on login."

disable:
	systemctl --user disable --now argus-lasso.service
	@echo "Autostart disabled."
