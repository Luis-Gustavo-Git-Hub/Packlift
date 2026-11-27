PREFIX ?= /usr/local
APP_ID = io.github.luis_gustavo_git_hub.Packlift
SHARE = $(DESTDIR)$(PREFIX)/share

.PHONY: build install test

build:
	cargo build --locked --release --features gui

test:
	cargo test --locked --all-features

install: build
	install -Dm755 target/release/packlift "$(DESTDIR)$(PREFIX)/bin/packlift"
	ln -sfn packlift "$(DESTDIR)$(PREFIX)/bin/flatapk"
	install -Dm644 data/$(APP_ID).desktop "$(SHARE)/applications/$(APP_ID).desktop"
	install -Dm644 data/$(APP_ID).service "$(SHARE)/dbus-1/services/$(APP_ID).service"
	sed -i 's|Exec=/app/bin/packlift|Exec=$(PREFIX)/bin/packlift|' "$(SHARE)/dbus-1/services/$(APP_ID).service"
	install -Dm644 data/$(APP_ID).search-provider.ini "$(SHARE)/gnome-shell/search-providers/$(APP_ID).search-provider.ini"
	install -Dm644 data/$(APP_ID).metainfo.xml "$(SHARE)/metainfo/$(APP_ID).metainfo.xml"
	install -Dm644 data/$(APP_ID).xml "$(SHARE)/mime/packages/$(APP_ID).xml"
	install -Dm644 data/$(APP_ID).png "$(SHARE)/icons/hicolor/256x256/apps/$(APP_ID).png"
