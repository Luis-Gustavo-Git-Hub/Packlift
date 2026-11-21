PREFIX ?= /usr/local
APP_ID = io.github.flatapk.Installer
SHARE = $(DESTDIR)$(PREFIX)/share

.PHONY: build install test

build:
	cargo build --locked --release --features gui

test:
	python3 -m unittest discover -s tests -v

install: build
	install -Dm755 target/release/packlift "$(DESTDIR)$(PREFIX)/bin/flatapk"
	install -Dm644 data/$(APP_ID).desktop "$(SHARE)/applications/$(APP_ID).desktop"
	install -Dm644 data/$(APP_ID).metainfo.xml "$(SHARE)/metainfo/$(APP_ID).metainfo.xml"
	install -Dm644 data/$(APP_ID).xml "$(SHARE)/mime/packages/$(APP_ID).xml"
	install -Dm644 data/$(APP_ID).png "$(SHARE)/icons/hicolor/128x128/apps/$(APP_ID).png"
