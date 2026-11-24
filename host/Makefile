CXX ?= g++
ifeq ($(OS),Windows_NT)
BACKENDS ?= windows
endif
ifeq ($(shell uname -s),Darwin)
BACKENDS ?= macos
PLATFORM_PKGS = openssl
endif
BACKENDS ?= x11
HAS_WAYLAND = $(filter wayland,$(BACKENDS))
AVAILABLE_BACKENDS = x11 wayland windows macos
# Both Linux backends capture desktop audio from the PulseAudio (or PipeWire-Pulse) default sink monitor.
BACKEND_PKGS_x11 = x11 xext xdamage xfixes xrandr xtst libpulse-simple
BACKEND_DIR_x11 = linux/x11 linux/pulse
BACKEND_PKGS_wayland = wayland-client xkbcommon libpulse-simple
BACKEND_DIR_wayland = linux/wayland linux/pulse
BACKEND_DIR_windows = windows
BACKEND_LIBS_windows = -ld3d11 -ldxgi -lbcrypt -lgdi32 -luser32 -lws2_32 -lmfplat -lmfuuid -lole32 -lmmdevapi
BACKEND_FLAGS_windows = -DNOMINMAX -D_WIN32_WINNT=0x0A00
BACKEND_DIR_macos = macos
BACKEND_FLAGS_macos = -mmacosx-version-min=12.3 -Werror=unguarded-availability-new
BACKEND_LIBS_macos = -framework ScreenCaptureKit -framework VideoToolbox -framework CoreMedia -framework CoreVideo -framework CoreGraphics -framework ApplicationServices -framework AppKit -framework Foundation
ifneq ($(filter-out $(AVAILABLE_BACKENDS),$(BACKENDS)),)
$(error Unknown BACKENDS '$(filter-out $(AVAILABLE_BACKENDS),$(BACKENDS))'; available: $(AVAILABLE_BACKENDS))
endif
ifeq ($(strip $(BACKENDS)),)
$(error BACKENDS must name at least one backend; available: $(AVAILABLE_BACKENDS))
endif
BACKEND_PKGS = $(sort $(foreach backend,$(BACKENDS),$(BACKEND_PKGS_$(backend))))
BACKEND_DIRS = $(sort $(foreach backend,$(BACKENDS),$(BACKEND_DIR_$(backend))))
BACKEND_CPPFLAGS = $(foreach backend,$(sort $(BACKENDS)),-DRELAY_BACKEND_$(backend) $(BACKEND_FLAGS_$(backend)))
CODEC_PKGS = x264 opus
# Linux hardware encoding (NVENC, VAAPI) goes through libavcodec when it is installed; macOS and Windows use the
# system VideoToolbox and Media Foundation encoders and never link FFmpeg.
ifneq ($(filter x11 wayland,$(BACKENDS)),)
ifeq ($(shell uname -s),Linux)
ifeq ($(shell pkg-config --exists libavcodec libavutil && echo yes),yes)
VIDEO_ENCODER_CPPFLAGS = -DSKYLATCH_FFMPEG_ENCODER $(shell pkg-config --cflags libavcodec libavutil)
VIDEO_ENCODER_LIBS = $(shell pkg-config --libs libavcodec libavutil)
endif
endif
endif
PKGS = $(sort $(BACKEND_PKGS) $(CODEC_PKGS) $(PLATFORM_PKGS) libwebsockets zlib)
CPPFLAGS += -Isrc $(BACKEND_CPPFLAGS) $(VIDEO_ENCODER_CPPFLAGS) $(shell pkg-config --cflags $(PKGS))
CXXFLAGS ?= -O2
CXXFLAGS += -std=c++17 -Wall -Wextra -Wpedantic -pthread
BACKEND_LIBS = $(foreach backend,$(BACKENDS),$(BACKEND_LIBS_$(backend)))
LDLIBS += $(shell pkg-config --libs $(PKGS)) $(VIDEO_ENCODER_LIBS) $(BACKEND_LIBS) -pthread
# Unit tests that link the core sources need the encoder libraries the engine links.
CORE_CPPFLAGS = -Isrc $(BACKEND_CPPFLAGS) $(VIDEO_ENCODER_CPPFLAGS)
CORE_LIBS = $(VIDEO_ENCODER_LIBS) $(BACKEND_LIBS)

CORE_SRCS = $(wildcard src/core/*.cpp)
BACKEND_SRCS = $(foreach dir,$(BACKEND_DIRS),$(wildcard src/backends/$(dir)/*.cpp src/backends/$(dir)/*.mm))
BACKEND_OBJCXXFLAGS = $(if $(filter %.mm,$(BACKEND_SRCS)),-fobjc-arc)
ENGINE_SRCS = $(CORE_SRCS) $(wildcard src/server/*.cpp) $(BACKEND_SRCS)
HEADERS = $(wildcard src/*/*.hpp src/backends/linux/*.hpp) $(foreach dir,$(BACKEND_DIRS),$(wildcard src/backends/$(dir)/*.hpp))

WAYLAND_PROTOCOLS = wlr-data-control-unstable-v1 wlr-screencopy-unstable-v1 wlr-virtual-pointer-unstable-v1 virtual-keyboard-unstable-v1
.PHONY: all deps install test test-audio test-color test-flow test-capture-bucket test-cpu-budget test-surface-budget test-tile-diff test-capture-resize test-encoder test-png-fit test-input test-backend test-server test-keymap test-macos-keys test-macos-cursor clean FORCE
all: skylatch-host-engine

ifeq ($(HAS_WAYLAND),wayland)
WAYLAND_SCANNER = $(shell pkg-config --variable=wayland_scanner wayland-scanner)
WAYLAND_PROTOCOL_HEADERS = $(addprefix build/protocols/,$(addsuffix -client-protocol.h,$(WAYLAND_PROTOCOLS)))
WAYLAND_PROTOCOL_SOURCES = $(addprefix build/protocols/,$(addsuffix -protocol.c,$(WAYLAND_PROTOCOLS)))
WAYLAND_PROTOCOL_OBJECTS = $(addprefix build/protocols/,$(addsuffix -protocol.o,$(WAYLAND_PROTOCOLS)))
CPPFLAGS += -Ibuild/protocols

$(WAYLAND_PROTOCOL_HEADERS): build/protocols/%-client-protocol.h: src/backends/linux/wayland/protocols/%.xml
	mkdir -p $(@D)
	$(WAYLAND_SCANNER) client-header $< $@

$(WAYLAND_PROTOCOL_SOURCES): build/protocols/%-protocol.c: src/backends/linux/wayland/protocols/%.xml
	mkdir -p $(@D)
	$(WAYLAND_SCANNER) private-code $< $@

$(WAYLAND_PROTOCOL_OBJECTS): build/protocols/%-protocol.o: build/protocols/%-protocol.c build/protocols/%-client-protocol.h
	$(CC) $(CPPFLAGS) $(CFLAGS) -c $< -o $@

$(ENGINE_SRCS): $(WAYLAND_PROTOCOL_HEADERS)
endif

deps:
	@$(CXX) --version >/dev/null 2>&1
	@pkg-config --exists --print-errors $(PKGS) $(if $(HAS_WAYLAND),wayland-scanner)

PREFIX ?= /usr/local

install: skylatch-host-engine
	install -d "$(DESTDIR)$(PREFIX)/bin"
	install -m 0755 skylatch-host-engine "$(DESTDIR)$(PREFIX)/bin/skylatch-host-engine"

build/backends: FORCE
	mkdir -p build
	@printf '%s\n' '$(strip $(BACKENDS))' | cmp -s - $@ || printf '%s\n' '$(strip $(BACKENDS))' > $@

FORCE:

skylatch-host-engine: $(ENGINE_SRCS) $(HEADERS) $(WAYLAND_PROTOCOL_OBJECTS) build/backends
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) $(BACKEND_OBJCXXFLAGS) $(ENGINE_SRCS) $(WAYLAND_PROTOCOL_OBJECTS) -o $@ $(LDLIBS)

build/color-test: tests/color-test.cpp src/core/color.cpp src/core/color.hpp
	mkdir -p build
	$(CXX) -Isrc $(CXXFLAGS) tests/color-test.cpp src/core/color.cpp -o $@

test-color: build/color-test
	./build/color-test

build/flow-test: tests/flow-test.cpp src/core/flow.hpp
	mkdir -p build
	$(CXX) -Isrc $(CXXFLAGS) tests/flow-test.cpp -o $@

test-flow: build/flow-test
	./build/flow-test

build/capture-bucket-test: tests/capture-bucket-test.cpp src/core/capture-bucket.hpp
	mkdir -p build
	$(CXX) -Isrc $(CXXFLAGS) tests/capture-bucket-test.cpp -o $@

test-capture-bucket: build/capture-bucket-test
	./build/capture-bucket-test

build/cpu-budget-test: tests/cpu-budget-test.cpp src/core/cpu-budget.hpp
	mkdir -p build
	$(CXX) -Isrc $(CXXFLAGS) tests/cpu-budget-test.cpp -o $@

test-cpu-budget: build/cpu-budget-test
	./build/cpu-budget-test

build/surface-budget-test: tests/surface-budget-test.cpp src/core/surface-budget.hpp src/core/image-scale.hpp src/core/scaled-capture.hpp src/core/capture-source.hpp
	mkdir -p build
	$(CXX) -Isrc $(CXXFLAGS) tests/surface-budget-test.cpp -o $@

test-surface-budget: build/surface-budget-test
	./build/surface-budget-test

build/tile-diff-test: tests/tile-diff-test.cpp src/core/tile-diff.hpp src/core/capture-source.hpp
	mkdir -p build
	$(CXX) -Isrc $(CXXFLAGS) tests/tile-diff-test.cpp -o $@

test-tile-diff: build/tile-diff-test
	./build/tile-diff-test

build/png-fit-test: tests/png-fit-test.cpp src/core/png-fit.hpp src/core/image-scale.hpp src/core/capture-source.hpp
	mkdir -p build
	$(CXX) -Isrc $(CXXFLAGS) tests/png-fit-test.cpp -o $@ $(shell pkg-config --cflags --libs zlib)

test-png-fit: build/png-fit-test
	./build/png-fit-test

build/input-test: tests/input-test.cpp src/core/input.cpp src/core/input.hpp
	mkdir -p build
	$(CXX) -Isrc $(CXXFLAGS) tests/input-test.cpp src/core/input.cpp -o $@

test-input: build/input-test
	./build/input-test

build/backend-source-test: tests/backend-source-test.cpp $(CORE_SRCS) $(wildcard src/core/*.hpp) build/backends
	mkdir -p build
	$(CXX) $(CORE_CPPFLAGS) $(CXXFLAGS) tests/backend-source-test.cpp $(CORE_SRCS) -o $@ $(shell pkg-config --cflags --libs $(CODEC_PKGS) libavcodec libavutil libswscale) $(CORE_LIBS)

test-backend: build/backend-source-test
	./build/backend-source-test

build/capture-resize-test: tests/capture-resize-test.cpp $(CORE_SRCS) $(wildcard src/core/*.hpp) build/backends
	mkdir -p build
	$(CXX) $(CORE_CPPFLAGS) $(CXXFLAGS) tests/capture-resize-test.cpp $(CORE_SRCS) -o $@ $(shell pkg-config --cflags --libs $(CODEC_PKGS)) $(CORE_LIBS)

build/encoder-test: tests/encoder-test.cpp $(CORE_SRCS) $(wildcard src/core/*.hpp) build/backends
	mkdir -p build
	$(CXX) $(CORE_CPPFLAGS) $(CXXFLAGS) tests/encoder-test.cpp $(CORE_SRCS) -o $@ $(shell pkg-config --cflags --libs $(CODEC_PKGS) libavcodec libavutil) $(CORE_LIBS)

test-encoder: build/encoder-test
	./build/encoder-test

test-capture-resize: build/capture-resize-test
	./build/capture-resize-test

SERVER_TEST_SRCS = src/server/clipboard.cpp src/server/config.cpp src/server/metrics.cpp src/server/wire.cpp src/server/backend.cpp src/core/input.cpp src/core/wait.cpp $(BACKEND_SRCS)

build/test-viewer/viewer.js:
	mkdir -p build/test-viewer
	: > build/test-viewer/index.html
	: > build/test-viewer/viewer.js

build/server-test: tests/server-test.cpp $(SERVER_TEST_SRCS) $(HEADERS) $(WAYLAND_PROTOCOL_HEADERS) $(WAYLAND_PROTOCOL_OBJECTS) build/test-viewer/viewer.js build/backends
	mkdir -p build
	$(CXX) -Isrc $(BACKEND_CPPFLAGS) $(if $(HAS_WAYLAND),-Ibuild/protocols) $(shell pkg-config --cflags $(BACKEND_PKGS) $(PLATFORM_PKGS) libwebsockets) $(CXXFLAGS) $(BACKEND_OBJCXXFLAGS) tests/server-test.cpp $(SERVER_TEST_SRCS) $(WAYLAND_PROTOCOL_OBJECTS) -o $@ $(shell pkg-config --libs $(BACKEND_PKGS) $(PLATFORM_PKGS) libwebsockets) $(BACKEND_LIBS)

test-server: build/server-test
	./build/server-test

build/audio-test: tests/audio-test.cpp src/core/audio-encoder.cpp $(SERVER_TEST_SRCS) $(HEADERS) $(WAYLAND_PROTOCOL_HEADERS) $(WAYLAND_PROTOCOL_OBJECTS) build/test-viewer/viewer.js build/backends
	mkdir -p build
	$(CXX) -Isrc $(BACKEND_CPPFLAGS) $(if $(HAS_WAYLAND),-Ibuild/protocols) $(shell pkg-config --cflags $(BACKEND_PKGS) $(PLATFORM_PKGS) libwebsockets opus) $(CXXFLAGS) $(BACKEND_OBJCXXFLAGS) tests/audio-test.cpp src/core/audio-encoder.cpp $(SERVER_TEST_SRCS) $(WAYLAND_PROTOCOL_OBJECTS) -o $@ $(shell pkg-config --libs $(BACKEND_PKGS) $(PLATFORM_PKGS) libwebsockets opus) $(BACKEND_LIBS)

test-audio: build/audio-test
	./build/audio-test

build/keymap-test: tests/keymap-test.cpp src/backends/linux/wayland/keymap.cpp
	mkdir -p build
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) $(shell pkg-config --cflags xkbcommon) $^ -o $@ $(shell pkg-config --libs xkbcommon)

test-keymap: build/keymap-test
	./build/keymap-test

build/macos-keys-test: tests/macos-keys-test.cpp src/backends/macos/keys.hpp src/backends/linux/keycodes.hpp
	mkdir -p build
	$(CXX) -Isrc $(CXXFLAGS) tests/macos-keys-test.cpp -o $@ -framework Carbon -framework CoreFoundation

test-macos-keys: build/macos-keys-test
	./build/macos-keys-test

build/macos-cursor-test: tests/macos-cursor-test.cpp src/backends/macos/cursor.hpp src/core/capture-source.hpp
	mkdir -p build
	$(CXX) -Isrc $(CXXFLAGS) tests/macos-cursor-test.cpp -o $@

test-macos-cursor: build/macos-cursor-test
	./build/macos-cursor-test

test: test-audio test-color test-flow test-capture-bucket test-cpu-budget test-surface-budget test-tile-diff test-capture-resize test-encoder test-png-fit test-input test-backend test-server $(if $(HAS_WAYLAND),test-keymap) $(if $(filter macos,$(BACKENDS)),test-macos-keys test-macos-cursor)

clean:
	rm -rf build skylatch-host-engine
