# SPDX-License-Identifier: Apache-2.0
CXX ?= c++
CARGO ?= cargo
PKG_CONFIG ?= pkg-config
CORE_LIB_DIR ?= ../../target/release
CORE_LIB := $(CORE_LIB_DIR)/libskylatch_client_core.a
CORE_INCLUDE := ../../crates/skylatch-client-core/include
PACKAGES := sdl2 libavcodec libavutil json-c
WARNINGS := -std=c++20 -Wall -Wextra -Wpedantic
CXXFLAGS ?= -O2
SOURCES := client.cpp logic.cpp decoder.cpp
HEADERS := logic.hpp decoder.hpp $(CORE_INCLUDE)/skylatch_client_core.h
UNAME := $(shell uname -s)
ifeq ($(UNAME),Darwin)
export MACOSX_DEPLOYMENT_TARGET ?= $(shell sw_vers -productVersion)
CORE_SYSTEM_LIBS := -framework Security -framework CoreFoundation -framework SystemConfiguration -liconv
else
CORE_SYSTEM_LIBS := -pthread -ldl -lm -lrt -lutil
endif

.PHONY: all check-deps test clean
all: skylatch-client

ifeq ($(SKIP_RUST),1)
$(CORE_LIB):
	@echo 'Missing $@; run build.sh without --skip-rust.' >&2; exit 1
else
.PHONY: FORCE
$(CORE_LIB): FORCE
	$(CARGO) build --locked --release -p skylatch-client-core --manifest-path ../../Cargo.toml --target-dir ../../target
endif

check-deps:
	$(PKG_CONFIG) --print-errors --exists 'sdl2 >= 2.0.18' libavcodec libavutil json-c

skylatch-client: Makefile $(SOURCES) $(HEADERS) $(CORE_LIB) | check-deps
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) $(WARNINGS) -I$(CORE_INCLUDE) $$($(PKG_CONFIG) --cflags $(PACKAGES)) $(SOURCES) $(LDFLAGS) $(CORE_LIB) $$($(PKG_CONFIG) --libs $(PACKAGES)) $(CORE_SYSTEM_LIBS) $(LDLIBS) -o $@

build/logic-tests: Makefile tests/logic.cpp logic.cpp logic.hpp
	mkdir -p build
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) $(WARNINGS) -I. tests/logic.cpp logic.cpp $(LDFLAGS) $(LDLIBS) -o $@

test: build/logic-tests
	./build/logic-tests

clean:
	rm -f skylatch-client build/logic-tests
