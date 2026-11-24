#include "core/cpu-budget.hpp"

#include <cassert>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <string>
#include <vector>

int main() {
  using namespace relay;

  const std::string v2_mount =
      "30 25 0:26 / /sys/fs/cgroup rw,nosuid,nodev,noexec,relatime shared:4 - cgroup2 cgroup2 "
      "rw,nsdelegate,memory_recursiveprot\n";
  const std::string v1_mount =
      "36 35 0:31 / /sys/fs/cgroup/cpu,cpuacct rw,nosuid,nodev,noexec,relatime shared:9 - cgroup "
      "cgroup rw,cpu,cpuacct\n";

  assert(cgroupFiles(v2_mount, "0::/\n", true, "cpu.max").back() == "/sys/fs/cgroup/cpu.max");
  assert(cgroupFiles(v2_mount, "0::/docker/abc\n", true, "cpu.max").back() ==
         "/sys/fs/cgroup/docker/abc/cpu.max");
  assert(cgroupFiles("30 25 0:26 /docker/abc /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n",
                     "0::/docker/abc\n", true, "cpu.max").back() == "/sys/fs/cgroup/cpu.max");
  assert(cgroupFiles(v1_mount, "4:cpu,cpuacct:/docker/abc\n", false, "cpu.cfs_quota_us").back() ==
         "/sys/fs/cgroup/cpu,cpuacct/docker/abc/cpu.cfs_quota_us");
  assert(cgroupFiles("36 35 0:31 /docker/abc /sys/fs/cgroup/cpu rw - cgroup cgroup rw,cpu\n",
                     "4:cpu:/docker/abc\n", false, "cpu.cfs_quota_us").back() ==
         "/sys/fs/cgroup/cpu/cpu.cfs_quota_us");
  assert(cgroupFiles("37 35 0:32 / /sys/fs/cgroup/cpuset rw - cgroup cgroup rw,cpuset\n",
                     "5:cpuset:/docker/abc\n", false, "cpu.cfs_quota_us").empty());
  assert(cgroupFiles(v2_mount, "1:cpu,cpuacct:/docker/abc\n0::/system.slice/relay.scope\n", true, "cpu.max").back() ==
         "/sys/fs/cgroup/system.slice/relay.scope/cpu.max");
  assert(cgroupFiles("30 25 0:26 /docker/abc /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n",
                     "0::/docker/abc/child\n", true, "cpu.max").back() == "/sys/fs/cgroup/child/cpu.max");
  assert(cgroupFiles("36 35 0:31 /docker/abc /sys/fs/cgroup/cpu rw - cgroup cgroup rw,cpu\n",
                     "4:cpu:/docker/abcd\n", false, "cpu.cfs_quota_us").empty());

  const std::vector<std::string> chain = cgroupFiles(v2_mount, "0::/system.slice/relay.scope\n", true, "cpu.max");
  assert(chain.size() == 3);
  assert(chain[0] == "/sys/fs/cgroup/cpu.max");
  assert(chain[1] == "/sys/fs/cgroup/system.slice/cpu.max");
  assert(chain[2] == "/sys/fs/cgroup/system.slice/relay.scope/cpu.max");
  const std::vector<std::string> v1_chain = cgroupFiles(
      "36 35 0:31 /docker/abc /sys/fs/cgroup/cpu rw - cgroup cgroup rw,cpu\n", "4:cpu:/docker/abc\n", false,
      "cpu.cfs_quota_us");
  assert(v1_chain.size() == 1 && v1_chain[0] == "/sys/fs/cgroup/cpu/cpu.cfs_quota_us");

  assert(decodeMountField("/docker/a\\040b") == "/docker/a b");
  assert(hasController("rw,cpu,cpuacct", "cpu"));
  assert(!hasController("rw,cpuset", "cpu"));
  assert(!hasController("rw,cpuacct", "cpu"));

  assert(quotaCpus(200000, 100000) == 2);
  assert(quotaCpus(150000, 100000) == 2);
  assert(quotaCpus(50000, 100000) == 1);
  assert(quotaCpus(0, 100000) == 0);
  assert(quotaCpus(200000, 0) == 0);
  assert(quotaCpus(UINT64_MAX, 1) == std::numeric_limits<unsigned>::max());
  assert(quotaCpusText("150000\n", "100000\n") == 2);
  assert(quotaCpusText("-1\n", "100000\n") == 0);
  assert(quotaCpusText("100000\n", "0\n") == 0);
  assert(quotaCpusText("18446744073709551615\n", "100000\n") == 0);
  assert(unifiedQuotaCpus("200000 100000\n") == 2);
  assert(unifiedQuotaCpus("max 100000\n") == 0);
  assert(unifiedQuotaCpus("") == 0);
  assert(unifiedQuotaCpus("18446744073709551615 100000\n") == 0);

  const std::filesystem::path root = std::filesystem::temp_directory_path() / "relay-cpu-budget-test";
  std::filesystem::remove_all(root);
  std::filesystem::create_directories(root);
  const std::string proc = root.generic_string();
  std::string mount = proc + "/sys/fs/cgroup";
  for (size_t i = 0; i < mount.size(); ++i) {
    if (mount[i] == ' ') {
      mount.replace(i, 1, "\\040");
      i += 3;
    }
  }
  const auto write = [](const std::filesystem::path& path, const std::string& text) {
    std::filesystem::create_directories(path.parent_path());
    std::ofstream file(path);
    file << text;
  };

  write(root / "mountinfo", "30 25 0:26 / " + mount + " rw - cgroup2 cgroup2 rw\n");
  write(root / "cgroup", "0::/system.slice/relay.scope\n");
  write(root / "sys" / "fs" / "cgroup" / "cpu.max", "200000 100000\n");
  write(root / "sys" / "fs" / "cgroup" / "system.slice" / "cpu.max", "max 100000\n");
  write(root / "sys" / "fs" / "cgroup" / "system.slice" / "relay.scope" / "cpu.max", "max 100000\n");
  assert(cgroupQuotaCpus(proc) == 2);

  std::filesystem::remove_all(root);
  std::filesystem::create_directories(root);
  write(root / "mountinfo", "36 35 0:31 / " + mount + " rw - cgroup cgroup rw,cpu,cpuacct\n");
  write(root / "cgroup", "4:cpu,cpuacct:/docker/abc\n");
  write(root / "sys" / "fs" / "cgroup" / "docker" / "abc" / "cpu.cfs_quota_us", "150000\n");
  write(root / "sys" / "fs" / "cgroup" / "docker" / "abc" / "cpu.cfs_period_us", "100000\n");
  assert(cgroupQuotaCpus(proc) == 2);

  std::filesystem::remove_all(root);
  std::filesystem::create_directories(root);
  write(root / "mountinfo", "36 35 0:31 /docker/abc " + mount + " rw - cgroup cgroup rw,cpu\n");
  write(root / "cgroup", "4:cpu:/docker/abc\n");
  write(root / "sys" / "fs" / "cgroup" / "cpu.cfs_quota_us", "100000\n");
  write(root / "sys" / "fs" / "cgroup" / "cpu.cfs_period_us", "100000\n");
  assert(cgroupQuotaCpus(proc) == 1);

  std::filesystem::remove_all(root);
  std::filesystem::create_directories(root);
  write(root / "mountinfo", "36 35 0:31 /docker/abc " + mount + " rw - cgroup cgroup rw,cpu\n");
  write(root / "cgroup", "4:cpu:/docker/abcd\n");
  write(root / "sys" / "fs" / "cgroup" / "docker" / "abcd" / "cpu.cfs_quota_us", "100000\n");
  write(root / "sys" / "fs" / "cgroup" / "docker" / "abcd" / "cpu.cfs_period_us", "100000\n");
  assert(cgroupQuotaCpus(proc) == 0);

  std::filesystem::remove_all(root);

  assert(availableCpus() >= 1);
  assert(encoderThreads(720) >= 1 && encoderThreads(720) <= 11);
  assert(encoderThreads(1080) >= 1 && encoderThreads(1080) <= 17);
  assert(encoderThreads(16) == 1);
  assert(sliceLimit(720) == 11);
  assert(sliceLimit(1080) == 17);

#if defined(__linux__)
  cpu_set_t original;
  CPU_ZERO(&original);
  if (sched_getaffinity(0, sizeof(original), &original) == 0 && CPU_COUNT(&original) > 0) {
    int first = -1;
    for (int cpu = 0; cpu < CPU_SETSIZE && first < 0; ++cpu) if (CPU_ISSET(cpu, &original)) first = cpu;
    cpu_set_t single;
    CPU_ZERO(&single);
    CPU_SET(first, &single);
    assert(sched_setaffinity(0, sizeof(single), &single) == 0);
    assert(affinityCpus() == 1);
    assert(availableCpus() == 1);
    assert(encoderThreads(720) == 1);
    assert(sched_setaffinity(0, sizeof(original), &original) == 0);
  }
#endif

  std::cout << "cpu budget checks passed\n";
}
