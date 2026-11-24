#include "core/capture-bucket.hpp"

#include <cassert>
#include <cstdint>
#include <iostream>

int main() {
  using relay::CaptureBucket;
  constexpr int64_t interval = 16666666;
  CaptureBucket bucket;
  bucket.reset(1000, interval);
  assert(bucket.ready(1000));
  assert(bucket.ready(1000));
  bucket.spend(1000);
  assert(bucket.ready(1000));
  bucket.spend(1000);
  assert(!bucket.ready(1000));
  assert(bucket.wait_ns(1000) == interval);

  assert(!bucket.ready(1000 + interval / 2));
  assert(bucket.wait_ns(1000 + interval / 2) == interval - interval / 2);
  assert(bucket.ready(1000 + interval));
  bucket.spend(1000 + interval);
  assert(bucket.wait_ns(1000 + interval) == interval);

  assert(bucket.ready(1000 + 10 * interval));
  assert(bucket.ready(1000 + 10 * interval));
  bucket.spend(1000 + 10 * interval);
  assert(bucket.ready(1000 + 10 * interval));
  bucket.spend(1000 + 10 * interval);
  assert(!bucket.ready(1000 + 10 * interval));
  assert(bucket.wait_ns(1000 + 10 * interval) == interval);
  assert(bucket.ready(1000 + 11 * interval));
  std::cout << "capture bucket checks passed\n";
}
