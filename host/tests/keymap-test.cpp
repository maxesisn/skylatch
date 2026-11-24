#include "backends/linux/wayland/keymap.hpp"
#include <xkbcommon/xkbcommon-keysyms.h>
#include <algorithm>
#include <cassert>
#include <cstdint>
#include <cstdlib>
#include <iostream>
#include <stdexcept>
#include <string>
#include <vector>

namespace {
/** Compiles keymap text and returns an evdev keycode's first-level keysym. */
xkb_keysym_t firstLevelKeysym(const std::string& text, uint32_t keycode) {
  relay::wayland::XkbContext context(xkb_context_new(XKB_CONTEXT_NO_FLAGS));
  assert(context);
  relay::wayland::XkbKeymap keymap(xkb_keymap_new_from_string(
      context.get(), text.c_str(), XKB_KEYMAP_FORMAT_TEXT_V1,
      XKB_KEYMAP_COMPILE_NO_FLAGS));
  assert(keymap);
  const xkb_keysym_t* symbols = nullptr;
  const int count = xkb_keymap_key_get_syms_by_level(keymap.get(), keycode + 8, 0, 0, &symbols);
  return count > 0 ? symbols[0] : XKB_KEY_NoSymbol;
}
}

int main() {
  setenv("XKB_DEFAULT_LAYOUT", "us", 1);
  const std::string base = relay::wayland::defaultKeymap();
  relay::wayland::Keymap keymap(base);
  const std::string original = keymap.text();
  bool changed = true;
  assert(keymap.assign('a', changed) == 30 && !changed && keymap.text() == original);
  const uint32_t codepoints[] = {0xe9, 0x20ac, 0x1f600};
  uint32_t spareKeycodes[3] = {};
  for (size_t index = 0; index < 3; ++index) {
    spareKeycodes[index] = keymap.assign(codepoints[index], changed);
    assert(spareKeycodes[index] != 0 && changed);
    assert(firstLevelKeysym(base, spareKeycodes[index]) == XKB_KEY_NoSymbol);
    assert(firstLevelKeysym(keymap.text(), spareKeycodes[index]) ==
           xkb_utf32_to_keysym(codepoints[index]));
  }
  assert(spareKeycodes[0] != spareKeycodes[1] && spareKeycodes[1] != spareKeycodes[2] &&
         spareKeycodes[0] != spareKeycodes[2]);
  const std::string assigned = keymap.text();
  assert(keymap.assign(codepoints[0], changed) == spareKeycodes[0] && !changed && keymap.text() == assigned);
  relay::wayland::Keymap full(base);
  std::vector<uint32_t> keycodes;
  uint32_t overflow = 0;
  for (size_t index = 0; index <= relay::wayland::spareSlots; ++index) {
    const uint32_t codepoint = 0x100 + uint32_t(index), keycode = full.assign(codepoint, changed);
    assert(keycode != 0 && changed);
    if (std::find(keycodes.begin(), keycodes.end(), keycode) != keycodes.end()) {
      assert(keycode == keycodes.front());
      overflow = codepoint;
      break;
    }
    keycodes.push_back(keycode);
  }
  assert(overflow != 0 && firstLevelKeysym(full.text(), keycodes.front()) == xkb_utf32_to_keysym(overflow));
  bool rejected = false;
  try { relay::wayland::Keymap invalid("not an xkb keymap"); }
  catch (const std::runtime_error&) { rejected = true; }
  assert(rejected);
  std::cout << "wayland spare keycode assignment passed\n";
}
