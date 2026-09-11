CC ?= cc
CFLAGS ?= -O2 -g
CFLAGS += -Wall -Wextra -Werror
PACKAGES = x11 xfixes xrender xext libsystemd
CPPFLAGS += $(shell pkg-config --cflags $(PACKAGES))
LDLIBS += $(shell pkg-config --libs $(PACKAGES))
PREFIX ?= /usr/local

all: neo-composited-cursor

neo-composited-cursor: neo-composited-cursor.c

check: neo-composited-cursor
	python3 test-composited-cursor.py ./neo-composited-cursor

install: neo-composited-cursor
	install -D -m 755 neo-composited-cursor $(DESTDIR)$(PREFIX)/bin/neo-composited-cursor
	install -D -m 755 neo-kwin-device-loss-recovery.py $(DESTDIR)$(PREFIX)/bin/neo-kwin-device-loss-recovery

clean:
	rm -f neo-composited-cursor

.PHONY: all check install clean
