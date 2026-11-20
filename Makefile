PREFIX ?= /usr/local
APP_ID = io.github.flatapk.Installer
APP_DIR = $(PREFIX)/lib/flatapk-app
SHARE = $(DESTDIR)$(PREFIX)/share

.PHONY: install test locale

locale:
	mkdir -p locale/pt_BR/LC_MESSAGES
	msgfmt --check -o locale/pt_BR/LC_MESSAGES/flatapk.mo po/pt_BR.po

test:
	python3 -m unittest discover -s tests -v

install: locale
	install -d "$(DESTDIR)$(APP_DIR)/flatapk" "$(DESTDIR)$(APP_DIR)/locale/pt_BR/LC_MESSAGES" "$(DESTDIR)$(PREFIX)/bin"
	install -m 644 flatapk/*.py "$(DESTDIR)$(APP_DIR)/flatapk"
	install -m 644 locale/pt_BR/LC_MESSAGES/flatapk.mo "$(DESTDIR)$(APP_DIR)/locale/pt_BR/LC_MESSAGES"
	sed 's|@APP_DIR@|$(APP_DIR)|g' data/flatapk.in > "$(DESTDIR)$(PREFIX)/bin/flatapk"
	chmod 755 "$(DESTDIR)$(PREFIX)/bin/flatapk"
	install -Dm644 data/$(APP_ID).desktop "$(SHARE)/applications/$(APP_ID).desktop"
	install -Dm644 data/$(APP_ID).metainfo.xml "$(SHARE)/metainfo/$(APP_ID).metainfo.xml"
	install -Dm644 data/$(APP_ID).xml "$(SHARE)/mime/packages/$(APP_ID).xml"
	install -Dm644 data/$(APP_ID).png "$(SHARE)/icons/hicolor/128x128/apps/$(APP_ID).png"
