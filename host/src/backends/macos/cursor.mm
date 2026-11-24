#include "backends/macos/cursor.hpp"
#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#include <vector>

namespace relay {
namespace macos {
CursorImage readSystemCursor(double scale) {
  CursorImage result;
  @autoreleasepool {
    NSCursor* cursor = [NSCursor currentSystemCursor];
    NSImage* image = cursor.image;
    if (!image) return result;
    const NSSize size = image.size;
    const NSPoint hot = cursor.hotSpot;
    CursorGeometry geometry;
    if (!cursorGeometry(size.width, size.height, hot.x, hot.y, scale, geometry)) return result;
    // Ask for the representation closest to the target pixel size, then resample into exactly that size.
    NSRect proposed = NSMakeRect(0, 0, geometry.width, geometry.height);
    CGImageRef source = [image CGImageForProposedRect:&proposed context:nil hints:nil];
    if (!source) return result;
    CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    if (!space) return result;
    std::vector<uint8_t> pixels(size_t(geometry.width) * geometry.height * 4, 0);
    CGContextRef context = CGBitmapContextCreate(pixels.data(), geometry.width, geometry.height, 8,
                                                 size_t(geometry.width) * 4, space,
                                                 kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big);
    CGColorSpaceRelease(space);
    if (!context) return result;
    CGContextSetInterpolationQuality(context, kCGInterpolationHigh);
    CGContextDrawImage(context, CGRectMake(0, 0, geometry.width, geometry.height), source);
    CGContextRelease(context);
    unpremultiplyRgba(pixels.data(), pixels.size() / 4);
    result.width = uint16_t(geometry.width);
    result.height = uint16_t(geometry.height);
    result.xhot = uint16_t(geometry.xhot);
    result.yhot = uint16_t(geometry.yhot);
    result.rgba = std::move(pixels);
  }
  return result;
}
}
}
