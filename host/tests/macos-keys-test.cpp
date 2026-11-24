#include "backends/linux/keycodes.hpp"
#include "backends/macos/keys.hpp"
#include <Carbon/Carbon.h>
#include <cassert>
#include <cstdint>
#include <iostream>

namespace {
/// Holds the US key layout and releases the input source list on exit.
struct UsLayout {
  CFArrayRef sources;
  const UCKeyboardLayout* table;
  ~UsLayout() { CFRelease(sources); }
};
UsLayout loadUsLayout() {
  const void* keys[] = {kTISPropertyInputSourceID};
  const void* values[] = {CFSTR("com.apple.keylayout.US")};
  CFDictionaryRef filter = CFDictionaryCreate(kCFAllocatorDefault, keys, values, 1,
                                              &kCFTypeDictionaryKeyCallBacks,
                                              &kCFTypeDictionaryValueCallBacks);
  CFArrayRef sources = TISCreateInputSourceList(filter, true);
  CFRelease(filter);
  assert(sources && CFArrayGetCount(sources) == 1);
  const auto source = (TISInputSourceRef)CFArrayGetValueAtIndex(sources, 0);
  const auto data = (CFDataRef)TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
  assert(data);
  return UsLayout{sources, (const UCKeyboardLayout*)CFDataGetBytePtr(data)};
}
/// Character the US layout types for a virtual key code, or 0 when it types none or several.
uint32_t typedCharacter(const UCKeyboardLayout* layout, int key, uint32_t keyboardType) {
  UInt32 deadKeys = 0;
  UniChar characters[4] = {};
  UniCharCount count = 0;
  const OSStatus status = UCKeyTranslate(layout, UInt16(key), kUCKeyActionDown, 0, keyboardType,
                                         kUCKeyTranslateNoDeadKeysMask, &deadKeys, 4, &count, characters);
  assert(status == noErr);
  return count == 1 ? characters[0] : 0;
}
void expectCharacter(const UsLayout& layout, uint32_t keyboardType, int hid, uint32_t expected) {
  const int key = relay::macos::macKey(relay::hidKeycode(uint16_t(hid)));
  const uint32_t actual = key < 0 ? 0 : typedCharacter(layout.table, key, keyboardType);
  if (key < 0 || actual != expected) {
    std::cerr << "HID usage " << hid << " types U+" << std::hex << actual << " through virtual key "
              << key << ", expected U+" << expected << std::dec << "\n";
  }
  assert(key >= 0 && actual == expected);
}
void expectKey(int hid, int expected) {
  const int key = relay::macos::macKey(relay::hidKeycode(uint16_t(hid)));
  if (key != expected) {
    std::cerr << "HID usage " << hid << " maps to virtual key " << key << ", expected " << expected << "\n";
  }
  assert(key == expected);
}
void expectUnmapped(int hid) {
  const int key = relay::macos::macKey(relay::hidKeycode(uint16_t(hid)));
  if (key >= 0) std::cerr << "HID usage " << hid << " maps to virtual key " << key << ", expected none\n";
  assert(key < 0);
}
}

int main() {
  const UsLayout layout = loadUsLayout();
  const uint32_t keyboardType = LMGetKbdType();
  for (int hid = 4; hid <= 29; ++hid) expectCharacter(layout, keyboardType, hid, uint32_t('a' + hid - 4));
  for (int hid = 30; hid <= 38; ++hid) expectCharacter(layout, keyboardType, hid, uint32_t('1' + hid - 30));
  expectCharacter(layout, keyboardType, 39, '0');
  expectCharacter(layout, keyboardType, 40, '\r');
  expectCharacter(layout, keyboardType, 41, 0x1b);
  expectCharacter(layout, keyboardType, 42, 0x08);
  expectCharacter(layout, keyboardType, 43, '\t');
  expectCharacter(layout, keyboardType, 44, ' ');
  expectCharacter(layout, keyboardType, 45, '-');
  expectCharacter(layout, keyboardType, 46, '=');
  expectCharacter(layout, keyboardType, 47, '[');
  expectCharacter(layout, keyboardType, 48, ']');
  expectCharacter(layout, keyboardType, 49, '\\');
  expectCharacter(layout, keyboardType, 51, ';');
  expectCharacter(layout, keyboardType, 52, '\'');
  expectCharacter(layout, keyboardType, 53, '`');
  expectCharacter(layout, keyboardType, 54, ',');
  expectCharacter(layout, keyboardType, 55, '.');
  expectCharacter(layout, keyboardType, 56, '/');
  expectCharacter(layout, keyboardType, 74, 0x01);
  expectCharacter(layout, keyboardType, 75, 0x0b);
  expectCharacter(layout, keyboardType, 76, 0x7f);
  expectCharacter(layout, keyboardType, 77, 0x04);
  expectCharacter(layout, keyboardType, 78, 0x0c);
  expectCharacter(layout, keyboardType, 79, 0x1d);
  expectCharacter(layout, keyboardType, 80, 0x1c);
  expectCharacter(layout, keyboardType, 81, 0x1f);
  expectCharacter(layout, keyboardType, 82, 0x1e);
  expectKey(84, kVK_ANSI_KeypadDivide);
  expectCharacter(layout, keyboardType, 85, '*');
  expectKey(86, kVK_ANSI_KeypadMinus);
  expectCharacter(layout, keyboardType, 87, '+');
  expectCharacter(layout, keyboardType, 88, 0x03);
  const int keypad[] = {kVK_ANSI_Keypad1, kVK_ANSI_Keypad2, kVK_ANSI_Keypad3, kVK_ANSI_Keypad4, kVK_ANSI_Keypad5,
                        kVK_ANSI_Keypad6, kVK_ANSI_Keypad7, kVK_ANSI_Keypad8, kVK_ANSI_Keypad9, kVK_ANSI_Keypad0,
                        kVK_ANSI_KeypadDecimal};
  for (int hid = 89; hid <= 99; ++hid) expectKey(hid, keypad[hid - 89]);
  expectKey(103, kVK_ANSI_KeypadEquals);
  expectKey(57, kVK_CapsLock);
  expectKey(58, kVK_F1);
  expectKey(59, kVK_F2);
  expectKey(60, kVK_F3);
  expectKey(61, kVK_F4);
  expectKey(62, kVK_F5);
  expectKey(63, kVK_F6);
  expectKey(64, kVK_F7);
  expectKey(65, kVK_F8);
  expectKey(66, kVK_F9);
  expectKey(67, kVK_F10);
  expectKey(68, kVK_F11);
  expectKey(69, kVK_F12);
  expectKey(73, kVK_Help);
  expectKey(83, kVK_ANSI_KeypadClear);
  expectKey(100, kVK_ISO_Section);
  expectKey(101, kVK_ContextualMenu);
  expectKey(104, kVK_F13);
  expectKey(105, kVK_F14);
  expectKey(106, kVK_F15);
  expectKey(107, kVK_F16);
  expectKey(108, kVK_F17);
  expectKey(109, kVK_F18);
  expectKey(110, kVK_F19);
  expectKey(111, kVK_F20);
  expectKey(224, kVK_Control);
  expectKey(225, kVK_Shift);
  expectKey(226, kVK_Option);
  expectKey(227, kVK_Command);
  expectKey(228, kVK_RightControl);
  expectKey(229, kVK_RightShift);
  expectKey(230, kVK_RightOption);
  expectKey(231, kVK_RightCommand);
  expectUnmapped(70);
  expectUnmapped(71);
  expectUnmapped(72);
  expectUnmapped(115);
  expectUnmapped(116);
  expectUnmapped(117);
  expectUnmapped(118);
  std::cout << "macOS key table matches the US keyboard layout\n";
}
