#include "core/flow.hpp"
#include <cassert>
#include <iostream>
#include <tuple>
#include <utility>

int main() {
  using relay::FlowControl;
  FlowControl f;
  assert(f.window == 1 && f.target_bitrate == 8000000);
  assert(!f.ack(1, 1));
  assert(!f.enqueue(2, 1, 10, 10));
  assert(!f.enqueue(1, 0, 10, 10));
  assert(!f.enqueue(1, FlowControl::max_bytes + 1, 10, 10));
  assert(f.enqueue(1, 100, 10, 10));
  assert(!f.ack(1, 20)); // Encoded but unsent is never credit.
  assert(f.sent(1, 20));
  assert(!f.ack(2, 30));
  assert(f.ack(1, 100020));
  assert(f.bytes == 0 && f.acknowledged == 1);
  assert(!f.ack(1, 100021));
  for (uint32_t seq = 2; seq < 10; ++seq) assert(f.enqueue(seq, 100, 100030, 100030));
  assert(f.enqueue(10, 100, 100030, 100030));
  assert(f.sent(2, 100040));
  assert(!f.sent(4, 100040));
  assert(!f.ack(3, 100050));
  assert(f.frames.size() == 9 && f.bytes == 900);
  assert(!f.paused(100030 + 100010 + 100000) && f.paused(100030 + 100010 + 200000));
  assert(!f.timedOut(3100029) && f.timedOut(3100030));
  assert(f.ack(2, 400000));
  assert(!f.timedOut(3100029) && f.timedOut(3100030)); // Trickle ACKs cannot retain an old tail.

  FlowControl sixty, oneTwenty;
  oneTwenty.configure(1000000, 8000000, 40000000, 120);
  assert(sixty.frameIntervalUs() == 16667 && oneTwenty.frameIntervalUs() == 8334);
  assert(sixty.enqueue(1, 100, 1, 1) && sixty.sent(1, 1) && sixty.ack(1, 45001));
  assert(oneTwenty.enqueue(1, 100, 1, 1) && oneTwenty.sent(1, 1) && oneTwenty.ack(1, 45001));

  const std::pair<unsigned, unsigned> fallback_limits[] = {{10, 30}, {30, 90}, {60, 180}, {120, 360}};
  for (const auto& [fps, limit] : fallback_limits) {
    FlowControl fallback_limit;
    fallback_limit.configure(1000000, 8000000, 40000000, fps);
    assert(fallback_limit.window == 1);
    assert(fallback_limit.enqueue(1, 1000, 1, 1) && fallback_limit.sent(1, 1));
    assert(fallback_limit.ack(1, 1000001));
    for (uint32_t seq = 2; seq <= limit + 1; ++seq) {
      assert(fallback_limit.enqueue(seq, 1000, 1000002, 1000002));
    }
    assert(!fallback_limit.enqueue(limit + 2, 1000, 1000002, 1000002));
    assert(fallback_limit.frameBitrate() == fallback_limit.target_bitrate);
    assert(fallback_limit.sent(2, 1000003) && fallback_limit.ack(2, 1000004));
    assert(fallback_limit.enqueue(limit + 2, 1000, 1000005, 1000005));
  }

  FlowControl cap;
  assert(cap.enqueue(1, FlowControl::max_bytes, 1, 1));
  assert(cap.paused(1));
  assert(!cap.enqueue(2, 1, 1, 1));
  assert(cap.sent(1, 2) && cap.ack(1, 3));
  assert(cap.bytes == 0 && cap.enqueue(2, 1, 4, 4));

  FlowControl congested;
  assert(congested.enqueue(1, 100000, 1, 1));
  assert(congested.enqueue(2, 100000, 1, 1));
  congested.update(500001);
  assert(congested.target_bitrate == 6000000);
  congested.update(500002);
  assert(congested.target_bitrate == 6000000); // Downshift hysteresis.
  for (uint64_t now = 1000001; now < 10000000; now += 500000) congested.update(now);
  assert(congested.target_bitrate == 1000000);

  FlowControl small;
  assert(small.enqueue(1, 100, 1, 1) && small.enqueue(2, 100, 1, 1));
  small.update(1000000);
  assert(small.target_bitrate == 8000000); // Tiny updates are not a bandwidth estimate.

  FlowControl idle;
  idle.update(900000000);
  assert(idle.target_bitrate == 8000000);
  for (uint32_t seq = 1; seq <= 20; ++seq) {
    const uint64_t now = uint64_t(seq) * 1000000;
    assert(idle.enqueue(seq, 200, now, now));
    assert(idle.sent(seq, now));
    assert(idle.ack(seq, now + 10000));
  }
  assert(idle.target_bitrate == 8000000);

  FlowControl recovery;
  for (uint32_t seq = 1; seq <= 180; ++seq) {
    const uint64_t now = uint64_t(seq) * 16667;
    assert(recovery.enqueue(seq, 20000, now, now));
    assert(recovery.sent(seq, now));
    assert(recovery.ack(seq, now + 5000));
  }
  assert(recovery.target_bitrate > 8000000 && recovery.target_bitrate <= 10648000);

  // ACKs arrive after 55ms while capture continues, matching an uncongested 50ms path.
  auto demand = [](FlowControl& flow, uint32_t count, size_t size, uint64_t spacing) {
    std::deque<std::pair<uint32_t, uint64_t>> pending;
    for (uint32_t seq = 1; seq <= count; ++seq) {
      const uint64_t now = uint64_t(seq) * spacing;
      while (!pending.empty() && pending.front().second <= now) {
        assert(flow.ack(pending.front().first, pending.front().second));
        pending.pop_front();
      }
      assert(flow.enqueue(seq, size, now, now) && flow.sent(seq, now));
      pending.emplace_back(seq, now + 55000);
    }
    for (const auto& ack : pending) assert(flow.ack(ack.first, ack.second));
  };
  FlowControl cappedQuality;
  cappedQuality.target_bitrate = 8800000;
  demand(cappedQuality, 180, 18000, 33333); // 4.32 Mbit/s is below 60% of the cap.
  assert(cappedQuality.target_bitrate > 8800000);

  FlowControl bounded;
  demand(bounded, 1800, 18000, 33333);
  assert(bounded.target_bitrate == 40000000);
  assert(bounded.enqueue(1801, 1000000, 61000000, 61000000));
  assert(bounded.enqueue(1802, 1000000, 61000000, 61000000));
  bounded.update(61500000);
  assert(bounded.target_bitrate < 40000000); // Probing cannot disable congestion backoff.

  FlowControl tinyContinuous;
  demand(tinyContinuous, 180, 200, 33333);
  assert(tinyContinuous.target_bitrate == 8000000);
  FlowControl discontinuous;
  demand(discontinuous, 60, 100000, 100000);
  assert(discontinuous.target_bitrate == 8000000);

  for (const uint64_t target : {8000000ULL, 17118698ULL}) {
    FlowControl queued;
    queued.target_bitrate = target;
    assert(queued.enqueue(1, 100000, 1, 1) && queued.sent(1, 1));
    assert(queued.ack(1, 5001));
    assert(queued.enqueue(2, 90000, 5002, 5002));
    assert(queued.enqueue(3, 90000, 5002, 5002));
    queued.update(550002);
    assert(queued.target_bitrate < target);
  }

  FlowControl slow;
  slow.configure(1000000, 8000000, 40000000, 10);
  demand(slow, 100, 18000, 100000);
  assert(slow.target_bitrate > 8000000);

  FlowControl timer;
  const auto idle_deadline = timer.serviceDeadlineUs(1, 0);
  assert(idle_deadline == 100001);
  assert(timer.enqueue(1, 1000, 2, 2));
  const auto active_deadline = timer.serviceDeadlineUs(2, idle_deadline);
  assert(active_deadline == 10002);
  assert(timer.serviceDeadlineUs(10001, active_deadline) == active_deadline);
  const auto rearmed_deadline = timer.serviceDeadlineUs(active_deadline, 0);
  assert(rearmed_deadline == 20002);
  assert(timer.sent(1, 10003) && timer.ack(1, 10004));
  assert(timer.serviceDeadlineUs(10005, rearmed_deadline) == rearmed_deadline);
  assert(timer.serviceDeadlineUs(rearmed_deadline, 0) == 120002);

  FlowControl::TransportSample sample;
  sample.min_rtt_us = 45000; sample.rtt_us = 50000;
  sample.delivery_rate_bps = 10000000; sample.bytes_acked = 10000;
  sample.snd_cwnd = 20;

  for (const unsigned fps : {10u, 30u, 60u, 120u}) {
    FlowControl capacity;
    capacity.configure(1000000, 8000000, 40000000, fps);
    auto capacity_sample = sample;
    capacity_sample.min_rtt_us = 2000000;
    capacity.sampleTransport(capacity_sample, 1);
    const unsigned limit = 3 * fps;
    assert(capacity.frameLimit() == limit && capacity.window == limit);
    assert(capacity.admissionAgeUs() == FlowControl::timeout_us);
    for (uint32_t seq = 1; seq <= limit; ++seq) {
      assert(!capacity.paused(1));
      assert(capacity.enqueue(seq, 1, 1, 1) && capacity.sent(seq, 1));
    }
    assert(capacity.paused(1) && !capacity.enqueue(limit + 1, 1, 1, 1));
    assert(capacity.ack(1, 1000001) && capacity.window == limit);
    assert(!capacity.paused(1000001) && capacity.enqueue(limit + 1, 1, 1000001, 1000001));
    assert(!capacity.timedOut(3000000) && capacity.timedOut(3000001));
  }

  for (const unsigned fps : {10u, 60u, 120u}) {
    FlowControl budget;
    budget.configure(4000000, 8000000, 40000000, fps);
    auto budget_sample = sample;
    budget_sample.delivery_rate_bps = 8000000;
    budget.sampleTransport(budget_sample, 1);
    const uint64_t interval = budget.frameIntervalUs();
    assert(!budget.paused(1) && budget.frameBitrate() == 8000000);
    budget_sample.notsent_bytes = interval - 1;
    budget.sampleTransport(budget_sample, 1);
    assert(budget.senderQueueUs() == interval - 1);
    assert(!budget.paused(1) && budget.frameBitrate() == 8000000);
    budget_sample.notsent_bytes = interval;
    budget.sampleTransport(budget_sample, 1);
    assert(!budget.paused(1) && budget.frameBitrate() == 8000000);
    budget_sample.notsent_bytes = 2 * interval;
    budget.sampleTransport(budget_sample, 1);
    assert(!budget.paused(1) && budget.frameBitrate() == 5000000);
    assert(budget.enqueue(1, 1, 1, 1) && budget.sent(1, 1));
    budget_sample.notsent_bytes = 3 * interval - 1;
    budget.sampleTransport(budget_sample, 1);
    assert(!budget.paused(1) && budget.frameBitrate() > 2000000 && budget.frameBitrate() < 5000000);
    budget_sample.notsent_bytes = 3 * interval;
    budget.sampleTransport(budget_sample, 1);
    assert(budget.paused(1) && budget.frameBitrate() == 2000000);
    assert(budget.frameBitrate() < budget.min_bitrate && budget.target_bitrate == 8000000);
    budget_sample.notsent_bytes = 3 * interval + 1;
    budget.sampleTransport(budget_sample, 1);
    assert(budget.paused(1) && budget.frameBitrate() == 2000000);
    budget_sample.notsent_bytes = 0;
    budget.sampleTransport(budget_sample, 1);
    assert(!budget.paused(1) && budget.frameBitrate() == 8000000);
  }

  FlowControl estimator;
  estimator.sampleTransport(sample, 0);
  auto lower = sample;
  lower.delivery_rate_bps = 6000000; lower.bytes_acked += 10000;
  estimator.sampleTransport(lower, 500000);
  assert(estimator.delivery_rate_bps == 10000000);
  lower.bytes_acked += 10000;
  estimator.sampleTransport(lower, 999999);
  assert(estimator.delivery_rate_bps == 10000000);
  estimator.update(1000000);
  assert(estimator.delivery_rate_bps == 6000000);
  lower.delivery_rate_bps = 20000000;
  estimator.sampleTransport(lower, 1200000);
  assert(estimator.delivery_rate_bps == 6000000);
  estimator.sampleTransport(lower, 1999999);
  assert(estimator.delivery_rate_bps == 0);

  FlowControl limited;
  auto app_limited = sample;
  app_limited.app_limited = true; app_limited.delivery_rate_bps = 1000000;
  limited.sampleTransport(app_limited, 1);
  app_limited.bytes_acked += 10000;
  limited.sampleTransport(app_limited, 500001);
  assert(limited.delivery_rate_bps == 0 && limited.target_bitrate > 8000000);
  auto eligible = sample;
  eligible.delivery_rate_bps = 6000000; eligible.bytes_acked = 30000;
  limited.sampleTransport(eligible, 500002);
  const auto limited_target = limited.target_bitrate;
  app_limited.delivery_rate_bps = 100000000;
  app_limited.bytes_acked = 40000;
  limited.sampleTransport(app_limited, 750002);
  limited.update(800002);
  assert(limited.delivery_rate_bps == 6000000 && limited.target_bitrate >= limited_target);
  const auto cached_target = limited.target_bitrate;
  app_limited.bytes_acked += 10000;
  limited.sampleTransport(app_limited, 1500002);
  assert(limited.delivery_rate_bps == 0 && limited.target_bitrate >= cached_target);

  for (const bool app_limited_sample : {true, false}) {
    FlowControl probing, sparse_probing;
    auto headroom = sample;
    headroom.app_limited = app_limited_sample;
    headroom.delivery_rate_bps = app_limited_sample ? 1000000 : 0;
    probing.sampleTransport(headroom, 1);
    sparse_probing.sampleTransport(headroom, 1);
    for (uint64_t elapsed = 100000; elapsed <= 8000000; elapsed += 100000) {
      const auto previous = probing.target_bitrate;
      headroom.bytes_acked += 10000;
      probing.sampleTransport(headroom, elapsed + 1);
      probing.update(elapsed + 1);
      assert(probing.target_bitrate >= previous && probing.target_bitrate <= probing.max_bitrate);
      if (elapsed % 500000 == 0) {
        sparse_probing.sampleTransport(headroom, elapsed + 1);
        assert(std::abs(double(probing.target_bitrate) - double(sparse_probing.target_bitrate)) < 100);
      }
      if (elapsed == 1000000) assert(std::abs(double(probing.target_bitrate) - 10000000.0) < 100);
    }
    assert(probing.target_bitrate == probing.max_bitrate);

    for (const uint64_t queued_us : {(probing.frameIntervalUs() + 1) / 2, probing.frameIntervalUs() + 1}) {
      FlowControl building;
      headroom.notsent_bytes = queued_us;
      building.sampleTransport(headroom, 1);
      assert(building.senderQueueUs() == queued_us);
      building.update(1000001);
      assert(building.target_bitrate == 8000000);
    }

    FlowControl stale;
    headroom.notsent_bytes = 0;
    stale.sampleTransport(headroom, 1);
    assert(stale.enqueue(1, 1000, 1, 1) && stale.sent(1, 1));
    assert(stale.senderQueueUs() == 0);
    stale.update(1 + stale.admissionAgeUs());
    stale.update(1000001);
    assert(stale.target_bitrate == 8000000);
  }

  FlowControl smoothed, frequent;
  auto fast = sample;
  fast.delivery_rate_bps = 20000000;
  smoothed.sampleTransport(fast, 0);
  frequent.sampleTransport(fast, 0);
  assert(smoothed.target_bitrate == 8000000);
  smoothed.update(500000);
  for (uint64_t now = 10000; now <= 500000; now += 10000) frequent.update(now);
  assert(smoothed.target_bitrate > 13600000 && smoothed.target_bitrate < 13800000);
  assert(std::abs(double(smoothed.target_bitrate) - double(frequent.target_bitrate)) < 100);
  assert(smoothed.notsentLowat() > 56000 && smoothed.notsentLowat() < 58000);

  FlowControl low_budget;
  low_budget.configure(1000000, 1000000, 40000000);
  assert(low_budget.notsentLowat() == 16384);

  FlowControl clamped;
  clamped.configure(4000000, 8000000, 12000000);
  fast.delivery_rate_bps = 100000000;
  clamped.sampleTransport(fast, 0);
  for (uint64_t now = 500000; now <= 10000000; now += 500000) {
    fast.bytes_acked += 10000;
    clamped.sampleTransport(fast, now);
    assert(clamped.target_bitrate >= 8000000 && clamped.target_bitrate <= 12000000);
  }
  assert(clamped.target_bitrate >= 11999000);
  fast.delivery_rate_bps = 1000000;
  for (uint64_t now = 10500000; now <= 22000000; now += 500000) {
    fast.bytes_acked += 10000;
    clamped.sampleTransport(fast, now);
    assert(clamped.target_bitrate >= 4000000 && clamped.target_bitrate <= 12000000);
  }
  assert(clamped.target_bitrate <= 4001000 && clamped.notsentLowat() == 16667);

  FlowControl queued;
  queued.sampleTransport(sample, 1);
  assert(queued.enqueue(1, 20000, 1, 1));
  assert(queued.senderQueueUs() == 16000 && !queued.paused(1));
  assert(queued.enqueue(2, 1000, 1, 1));
  assert(queued.senderQueueUs() == 16800 && !queued.paused(1));
  assert(queued.frameBitrate() < queued.target_bitrate);
  assert(queued.sent(1, 2));
  assert(queued.senderQueueUs() == 800 && !queued.paused(2));
  auto buffered = sample;
  buffered.notsent_bytes = 10000; buffered.application_queued_bytes = 10000;
  queued.sampleTransport(buffered, 3);
  assert(queued.senderQueueUs() == 16800 && !queued.paused(3));
  assert(queued.frameBitrate() < queued.target_bitrate);
  assert(queued.sent(2, 4));
  assert(queued.senderQueueUs() == 16000 && !queued.paused(4));
  queued.sampleTransport(sample, 5);
  assert(queued.bytes == 21000 && queued.senderQueueUs() == 0 && !queued.paused(5));

  FlowControl startup;
  auto zero_rate = sample;
  zero_rate.delivery_rate_bps = 0;
  startup.sampleTransport(zero_rate, 1);
  assert(startup.enqueue(1, 17000, 1, 1));
  assert(startup.target_bitrate == 8000000 && startup.delivery_rate_bps == 0);
  assert(startup.senderQueueUs() == 17000 && !startup.paused(1));
  assert(startup.frameBitrate() < startup.target_bitrate);
  assert(startup.sent(1, 2));
  assert(startup.senderQueueUs() == 0 && !startup.paused(2));

  FlowControl lossy;
  auto high_rtt = sample;
  high_rtt.min_rtt_us = 200000; high_rtt.rtt_us = 600000;
  high_rtt.total_retrans = 12;
  lossy.sampleTransport(high_rtt, 1);
  const auto lossy_window = lossy.window;
  for (uint32_t seq = 1; seq <= 12; ++seq) {
    assert(lossy.enqueue(seq, 100000, 1, 1) && lossy.sent(seq, 1));
  }
  lossy.update(300001);
  assert(lossy.window > 8 && lossy.window == lossy_window && !lossy.paused(300001));
  assert(lossy.target_bitrate >= 8000000);
  high_rtt.total_retrans += 10; high_rtt.rtt_us = 1000000;
  lossy.sampleTransport(high_rtt, 300002);
  assert(lossy.window == lossy_window && lossy.target_bitrate >= 8000000);
  assert(lossy.ack(1, 300003));
  assert(lossy.ack_rtt_us > 300000 && lossy.window == lossy_window && !lossy.paused(300003));
  assert(!lossy.paused(1 + lossy.admissionAgeUs() - 1));
  assert(lossy.paused(1 + lossy.admissionAgeUs()));

  FlowControl low_fps;
  low_fps.configure(1000000, 8000000, 40000000, 10);
  low_fps.sampleTransport(high_rtt, 1);
  assert(low_fps.admissionAgeUs() == 600000 && low_fps.window == 6);
  assert(lossy.admissionAgeUs() == 433334 && lossy.window == 26);
  FlowControl high_fps;
  high_fps.configure(1000000, 8000000, 40000000, 120);
  high_fps.sampleTransport(sample, 1);
  assert(high_fps.admissionAgeUs() == 106668 && high_fps.window == 13);
  assert(high_fps.notsentLowat() == 16667);
  assert(high_fps.enqueue(1, 1, 1, 1) && high_fps.sent(1, 1));
  assert(high_fps.ack(1, 500001) && high_fps.window == 13);
  high_fps.clearTransport();
  assert(high_fps.frameBitrate() == high_fps.target_bitrate);
  auto distant = sample;
  distant.min_rtt_us = 2000000;
  low_fps.sampleTransport(distant, 2);
  assert(low_fps.admissionAgeUs() == FlowControl::timeout_us && low_fps.window == 30);
  for (uint32_t seq = 1; seq <= 30; ++seq) assert(low_fps.enqueue(seq, 1, 2, 2));
  assert(!low_fps.enqueue(31, 1, 2, 2) && low_fps.paused(2));

  FlowControl fallback;
  fallback.configure(1000000, 8000000, 40000000, 10);
  fallback.sampleTransport(sample, 1);
  assert(fallback.enqueue(1, 100000, 1, 1) && fallback.sent(1, 1));
  assert(fallback.ack(1, 100001));
  const auto fallback_target = fallback.target_bitrate;
  fallback.clearTransport();
  assert(!fallback.transport_available && fallback.delivery_rate_bps == 0);
  assert(fallback.frameBitrate() == fallback.target_bitrate);
  assert(fallback.enqueue(2, 100000, 100002, 100002));
  assert(fallback.enqueue(3, 100000, 100002, 100002));
  assert(!fallback.paused(500001) && fallback.paused(500002));
  fallback.update(600002);
  assert(fallback.target_bitrate < fallback_target && fallback.target_bitrate >= fallback.min_bitrate);
  fallback.clearTransport();
  assert(fallback.frames.size() == 2 && fallback.target_bitrate < fallback_target);

  FlowControl kernel_cap;
  kernel_cap.sampleTransport(sample, 1);
  assert(kernel_cap.enqueue(1, FlowControl::max_bytes, 1, 1));
  assert(kernel_cap.paused(1) && !kernel_cap.enqueue(2, 1, 1, 1));
  assert(kernel_cap.sent(1, 2) && kernel_cap.ack(1, 3));
  assert(kernel_cap.enqueue(2, 1, 4, 4) && kernel_cap.sent(2, 4));
  kernel_cap.sampleTransport(sample, 3000003);
  assert(!kernel_cap.timedOut(3000003) && kernel_cap.timedOut(3000004));

  FlowControl session;
  session.configure(2000000, 9000000, 30000000, 10);
  session.sampleTransport(sample, 1);
  assert(session.enqueue(1, 1000, 1, 1) && session.sent(1, 2));
  session.reset();
  assert(!session.transport_available && session.delivery_rate_bps == 0);
  assert(session.transport.min_rtt_us == 0 && session.transport.bytes_acked == 0);
  assert(session.frames.empty() && session.bytes == 0 && session.window == 1);
  assert(session.frame_rate == 10 && session.target_bitrate == 9000000);
  assert(session.min_bitrate == 2000000 && session.max_bitrate == 30000000);
  assert(!session.timedOut(9000000) && session.enqueue(1, 1000, 9000001, 9000001));
  struct PathRun { unsigned skipped = 0, paused_ticks = 0; uint64_t worst_age = 0, min_bitrate = ~0ULL, bitrate_at_stall = 0; };
  auto simulate = [](FlowControl& flow, unsigned fps, uint64_t rtt, uint64_t refresh, uint64_t service_gap,
                     uint64_t seconds, uint64_t warmup_us, uint64_t burst = 0, uint64_t stall_at = 0,
                     uint64_t stall_us = 0, uint64_t viewer_wait = 0, bool lifetime_wait = false) {
    PathRun run;
    std::deque<std::tuple<uint32_t, uint64_t, uint64_t>> pending;
    uint64_t last_ack = 0, rng = 12345;
    uint32_t seq = 0, wait_from = ~0u;
    const uint64_t interval = 1000000 / fps;
    for (uint64_t now = 0; now < seconds * 1000000; now += interval) {
      while (!pending.empty() && std::get<1>(pending.front()) <= now) {
        const auto [acked, at, captured] = pending.front();
        const uint64_t wait = lifetime_wait ? at - captured : acked >= wait_from ? viewer_wait : 0;
        assert(flow.ack(acked, at, wait));
        pending.pop_front();
      }
      flow.update(now);
      run.worst_age = std::max(run.worst_age, flow.oldestAge(now));
      if (stall_us && now >= stall_at && !run.bitrate_at_stall) run.bitrate_at_stall = flow.target_bitrate;
      if (run.bitrate_at_stall) run.min_bitrate = std::min(run.min_bitrate, flow.target_bitrate);
      const bool admitted = !flow.paused(now) && flow.frames.size() < flow.window;
      if (!admitted) {
        if (now >= warmup_us) { ++run.skipped; run.paused_ticks += flow.paused(now); }
        continue;
      }
      assert(flow.enqueue(++seq, size_t(flow.target_bitrate / 8 / fps), now, now) && flow.sent(seq, now));
      if (now >= 2000000 && wait_from == ~0u) wait_from = seq;
      rng = rng * 6364136223846793005ULL + 1442695040888963407ULL;
      uint64_t at = now + rtt + (wait_from != ~0u ? viewer_wait : 0) + (rng >> 33) % (refresh + 1);
      if (burst) at = (at + burst - 1) / burst * burst;
      if (stall_us && at >= stall_at && at < stall_at + stall_us) at = stall_at + stall_us;
      if (service_gap) at = std::max(at, last_ack + service_gap);
      last_ack = at = std::max(at, last_ack);
      pending.emplace_back(seq, at, now);
    }
    return run;
  };
  for (const unsigned fps : {60u, 120u}) {
    for (const uint64_t rtt : {100000ULL, 150000ULL, 300000ULL}) {
      FlowControl clean;
      clean.configure(1000000, 8000000, 40000000, fps);
      const auto run = simulate(clean, fps, rtt, 16667, 0, 12, 2000000);
      assert(run.skipped == 0);
      assert(clean.target_bitrate > 16000000);
    }
  }

  for (const unsigned fps : {60u, 120u}) {
    FlowControl bursty;
    bursty.configure(1000000, 8000000, 40000000, fps);
    const uint64_t interval = 1000000 / fps;
    const auto run = simulate(bursty, fps, 100000, 0, 0, 20, 2000000, 5 * interval);
    assert(run.worst_age >= 100000 + 3 * interval);
    assert(run.skipped == 0 && run.paused_ticks == 0);
    assert(bursty.target_bitrate > 16000000);
  }

  for (const unsigned fps : {60u, 120u}) {
    FlowControl hiccup;
    hiccup.configure(1000000, 8000000, 40000000, fps);
    const auto run = simulate(hiccup, fps, 100000, 16667, 0, 30, 0, 0, 10000000, 300000);
    assert(run.min_bitrate > 4000000);
    assert(hiccup.target_bitrate >= run.bitrate_at_stall);
  }

  for (const unsigned fps : {60u, 120u}) {
    FlowControl waiting;
    waiting.configure(1000000, 8000000, 40000000, fps);
    const auto run = simulate(waiting, fps, 100000, 0, 0, 9, 2000000, 0, 0, 0, 80000);
    assert(run.paused_ticks == 0 && run.skipped <= fps / 10);
    assert(waiting.target_bitrate > 12000000);
  }

  for (const unsigned fps : {60u, 120u}) {
    FlowControl short_hold;
    short_hold.configure(1000000, 8000000, 40000000, fps);
    assert(simulate(short_hold, fps, 20000, 0, 0, 12, 2000000, 0, 0, 0, 80000).skipped <= fps / 10);
  }

  for (const unsigned fps : {60u, 120u}) {
    for (const uint64_t base : {5000ULL, 20000ULL}) {
      const uint64_t interval = 1000000 / fps;
      for (const uint64_t jitter : {interval, 2 * interval}) {
        FlowControl lan;
        lan.configure(1000000, 8000000, 40000000, fps);
        const auto run = simulate(lan, fps, base, jitter, 0, 12, 2000000);
        assert(run.skipped == 0 && run.paused_ticks == 0);
        assert(lan.target_bitrate >= 8000000);
      }
    }
  }

  for (const unsigned fps : {60u, 120u}) {
    FlowControl stalled;
    stalled.configure(1000000, 8000000, 40000000, fps);
    const auto run = simulate(stalled, fps, 100000, 16667, 0, 15, 0, 0, 5000000, 1500000);
    assert(run.paused_ticks > 0 && run.worst_age >= 1500000 && run.worst_age < FlowControl::timeout_us);
    assert(!stalled.paused(15000000) && stalled.acknowledged > 10ULL * fps);
  }

  FlowControl hidden;
  const auto hidden_run = simulate(hidden, 60, 100000, 16667, 40000, 20, 0, 0, 0, 0, 0, true);
  assert(hidden_run.paused_ticks > 0);
  assert(hidden_run.worst_age < 900000);

  FlowControl budget;
  for (uint32_t seq = 1; seq <= 4; ++seq) assert(!budget.paused(1) && budget.enqueue(seq, 1536 * 1024, 1, 1));
  assert(!budget.paused(1) && budget.enqueue(5, 1536 * 1024, 1, 1) && budget.paused(1));

  FlowControl untrusted;
  assert(untrusted.enqueue(1, 100, 1, 1) && untrusted.sent(1, 1));
  assert(untrusted.ack(1, 100001, 4294967295ULL));
  assert(untrusted.networkBaseDepthUs() == 100000 && untrusted.baseDepthUs() == 100000);
  assert(untrusted.enqueue(2, 100, 100002, 100002) && untrusted.sent(2, 100002));
  assert(untrusted.ack(2, 200002, 100000));
  assert(untrusted.networkBaseDepthUs() == 0 && untrusted.baseDepthUs() == 100000);
  const uint8_t four[] = {0, 0, 0x27, 0x10};
  assert(FlowControl::ackWaitUs(four, 4) == 10000 && FlowControl::ackWaitUs(four, 0) == 0 && FlowControl::ackWaitUs(four, 3) == 0);
  const uint8_t huge[] = {0xff, 0xff, 0xff, 0xff};
  assert(FlowControl::ackWaitUs(huge, 4) == 4294967295ULL);

  FlowControl growing;
  const auto grown = simulate(growing, 60, 100000, 16667, 40000, 20, 0);
  assert(grown.paused_ticks > 0 && growing.target_bitrate < 8000000);
  assert(grown.worst_age < 850000);

  for (const unsigned fps : {60u, 120u}) {
    FlowControl direct;
    direct.configure(1000000, 8000000, 40000000, fps);
    FlowControl::TransportSample link;
    link.min_rtt_us = 2000; link.rtt_us = 2000; link.delivery_rate_bps = 40000000; link.snd_cwnd = 20;
    const uint64_t interval = direct.frameIntervalUs(), hold = 60000;
    uint64_t now = 1, blocked = 0;
    uint32_t captured = 0, acked = 0;
    std::deque<uint64_t> shown_at;
    for (uint64_t tick = 0; tick < 3000000 / 1000; ++tick, now += 1000) {
      link.bytes_acked += 5000;
      direct.sampleTransport(link, now);
      while (!shown_at.empty() && shown_at.front() <= now) {
        assert(direct.ack(++acked, now, hold));
        shown_at.pop_front();
      }
      if (now >= uint64_t(captured) * interval + 1) {
        if (direct.paused(now) || shown_at.size() >= direct.window) { if (now > 1000000) ++blocked; continue; }
        assert(direct.enqueue(++captured, 5000, now, now) && direct.sent(captured, now));
        shown_at.push_back(now + 2000 + hold);
      }
    }
    assert(blocked == 0 && captured >= 3 * fps - 2);
  }

  std::cout << "flow policy checks passed\n";
}
