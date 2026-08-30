#!/usr/bin/env bash
set -euo pipefail

# Reproducible native-desktop resource benchmark for Linux/X11.
#
# Measures the entire process tree after an idle settle period. The Electron
# case is deliberately a blank local window: it isolates the minimum runtime
# overhead and is NOT presented as a measurement of an official WhatsApp app.

benchmark_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd -- "$benchmark_dir/../.." && pwd)
wasabi_bin=${WASABI_BIN:-"$repo_dir/apps/desktop/target/release/wasabi-desktop"}
electron_bin=${ELECTRON_BIN:-electron}
runs=${RUNS:-5}
settle_seconds=${SETTLE_SECONDS:-8}
cpu_sample_seconds=${CPU_SAMPLE_SECONDS:-3}
window_timeout_seconds=${WINDOW_TIMEOUT_SECONDS:-20}
output=${OUTPUT:-"$benchmark_dir/results-linux.csv"}
environment_output=${ENVIRONMENT_OUTPUT:-"$benchmark_dir/environment-linux.txt"}

if [[ ${XDG_SESSION_TYPE:-x11} != x11 && -z ${DISPLAY:-} ]]; then
  echo "benchmark requires an active X11 display" >&2
  exit 2
fi
if ! command -v xdotool >/dev/null; then
  echo "benchmark requires xdotool" >&2
  exit 2
fi
if [[ ! -x $wasabi_bin ]]; then
  echo "missing release binary: $wasabi_bin" >&2
  echo "run: cargo build --manifest-path apps/desktop/Cargo.toml --release" >&2
  exit 2
fi
if ! command -v "$electron_bin" >/dev/null; then
  echo "Electron not installed; set ELECTRON_BIN or install it" >&2
  exit 2
fi

benchmark_tmp=$(mktemp -d)
cleanup() {
  rm -rf -- "$benchmark_tmp"
}
trap cleanup EXIT
mkdir -p "$benchmark_tmp"

{
  printf 'measured_at_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'kernel=%s\n' "$(uname -sr)"
  printf 'architecture=%s\n' "$(uname -m)"
  printf 'cpu=%s\n' "$(awk -F: '/^model name/{sub(/^[ \t]+/, "", $2); print $2; exit}' /proc/cpuinfo)"
  printf 'logical_cpus=%s\n' "$(getconf _NPROCESSORS_ONLN)"
  printf 'memory_kib=%s\n' "$(awk '/^MemTotal:/{print $2; exit}' /proc/meminfo)"
  printf 'display_server=X11\n'
  printf 'wasabi_version=%s\n' "$(tr -d '[:space:]' < "$repo_dir/VERSION")"
  printf 'wasabi_binary_bytes=%s\n' "$(stat -c %s "$wasabi_bin")"
  electron_version=$("$electron_bin" --version 2>/dev/null || "$electron_bin" --no-sandbox --version 2>/dev/null || printf unavailable)
  printf 'electron_version=%s\n' "$(printf '%s' "$electron_version" | tr -d '\r')"
  printf 'electron_binary=%s\n' "$(readlink -f "$(command -v "$electron_bin")")"
} >"$environment_output"

process_tree() {
  local root=$1
  local -a queue=("$root")
  local -a found=()
  local at=0
  while (( at < ${#queue[@]} )); do
    local pid=${queue[$at]}
    at=$((at + 1))
    [[ -d /proc/$pid ]] || continue
    found+=("$pid")
    while read -r child; do
      [[ -n $child ]] && queue+=("$child")
    done < <(pgrep -P "$pid" 2>/dev/null || true)
  done
  printf '%s\n' "${found[@]}"
}

tree_ticks() {
  local total=0
  while read -r pid; do
    [[ -r /proc/$pid/stat ]] || continue
    local ticks
    ticks=$(awk '{ print $14 + $15 }' "/proc/$pid/stat")
    total=$((total + ticks))
  done < <(process_tree "$1")
  printf '%s\n' "$total"
}

find_window() {
  local root=$1
  local title=$2
  local -A members=()
  while read -r pid; do members[$pid]=1; done < <(process_tree "$root")
  while read -r window; do
    [[ -n $window ]] || continue
    local owner
    owner=$(xdotool getwindowpid "$window" 2>/dev/null || true)
    if [[ -n ${members[$owner]:-} ]]; then
      printf '%s\n' "$window"
      return 0
    fi
  done < <(xdotool search --name "^${title}$" 2>/dev/null || true)
  return 1
}

snapshot_tree() {
  local root=$1
  local rss=0 pss=0 threads=0 fds=0 processes=0
  while read -r pid; do
    [[ -d /proc/$pid ]] || continue
    processes=$((processes + 1))
    local value
    value=$(awk '/^VmRSS:/ { print $2; exit }' "/proc/$pid/status" 2>/dev/null || true)
    rss=$((rss + ${value:-0}))
    value=$(awk '/^Pss:/ { print $2; exit }' "/proc/$pid/smaps_rollup" 2>/dev/null || true)
    pss=$((pss + ${value:-0}))
    value=$(awk '/^Threads:/ { print $2; exit }' "/proc/$pid/status" 2>/dev/null || true)
    threads=$((threads + ${value:-0}))
    value=$(find "/proc/$pid/fd" -mindepth 1 -maxdepth 1 2>/dev/null | wc -l)
    fds=$((fds + value))
  done < <(process_tree "$root")
  printf '%s,%s,%s,%s,%s\n' "$rss" "$pss" "$threads" "$fds" "$processes"
}

stop_tree() {
  local root=$1
  kill -TERM -- "-$root" 2>/dev/null || true
  for _ in {1..20}; do
    if [[ ! -d /proc/$root ]]; then
      wait "$root" 2>/dev/null || true
      return 0
    fi
    sleep 0.1
  done
  kill -KILL -- "-$root" 2>/dev/null || true
  wait "$root" 2>/dev/null || true
}

benchmark_one() {
  local name=$1 title=$2
  shift 2
  local started_ns root window=""
  started_ns=$(date +%s%N)
  setsid "$@" >"$benchmark_tmp/${name}.log" 2>&1 &
  root=$!

  local polls=$((window_timeout_seconds * 20))
  for ((poll = 0; poll < polls; poll++)); do
    if window=$(find_window "$root" "$title"); then break; fi
    if ! kill -0 "$root" 2>/dev/null; then
      echo "$name exited before opening a window" >&2
      sed -n '1,120p' "$benchmark_tmp/${name}.log" >&2
      return 1
    fi
    sleep 0.05
  done
  if [[ -z $window ]]; then
    echo "$name did not open a window within ${window_timeout_seconds}s" >&2
    stop_tree "$root"
    return 1
  fi

  local opened_ns startup_ms
  opened_ns=$(date +%s%N)
  startup_ms=$(((opened_ns - started_ns) / 1000000))
  sleep "$settle_seconds"

  local before after clock_ticks cpu_percent snapshot
  clock_ticks=$(getconf CLK_TCK)
  before=$(tree_ticks "$root")
  sleep "$cpu_sample_seconds"
  after=$(tree_ticks "$root")
  cpu_percent=$(awk -v delta="$((after - before))" -v hz="$clock_ticks" -v secs="$cpu_sample_seconds" \
    'BEGIN { printf "%.2f", (delta / hz / secs) * 100 }')
  snapshot=$(snapshot_tree "$root")
  printf '%s,%s,%s,%s\n' "$name" "$startup_ms" "$cpu_percent" "$snapshot" >>"$output"
  stop_tree "$root"
}

printf 'app,startup_ms,idle_cpu_percent,rss_kib,pss_kib,threads,fds,processes\n' >"$output"

for ((run = 1; run <= runs; run++)); do
  run_root="$benchmark_tmp/run-$run"
  mkdir -p \
    "$run_root/wasabi-data" \
    "$run_root/wasabi-config" \
    "$run_root/wasabi-cache" \
    "$run_root/electron-config" \
    "$run_root/electron-cache"
  echo "run $run/$runs: Wasabi" >&2
  benchmark_one \
    wasabi \
    wasabi \
    env \
      XDG_DATA_HOME="$run_root/wasabi-data" \
      XDG_CONFIG_HOME="$run_root/wasabi-config" \
      XDG_CACHE_HOME="$run_root/wasabi-cache" \
      RUST_LOG=error \
      "$wasabi_bin"

  echo "run $run/$runs: Electron baseline" >&2
  benchmark_one \
    electron_blank \
    'Electron baseline' \
    env \
      XDG_CONFIG_HOME="$run_root/electron-config" \
      XDG_CACHE_HOME="$run_root/electron-cache" \
      "$electron_bin" \
      --no-sandbox \
      "$benchmark_dir/electron-baseline"
done

echo "raw results: $output" >&2
echo "environment: $environment_output" >&2
awk -F, '
  NR > 1 {
    count[$1]++
    startup[$1] += $2
    cpu[$1] += $3
    rss[$1] += $4
    pss[$1] += $5
    threads[$1] += $6
    fds[$1] += $7
    processes[$1] += $8
  }
  END {
    print "app,mean_startup_ms,mean_idle_cpu_percent,mean_rss_mib,mean_pss_mib,mean_threads,mean_fds,mean_processes"
    for (app in count) {
      n = count[app]
      printf "%s,%.1f,%.2f,%.1f,%.1f,%.1f,%.1f,%.1f\n", app,
        startup[app] / n, cpu[app] / n, rss[app] / n / 1024,
        pss[app] / n / 1024, threads[app] / n, fds[app] / n,
        processes[app] / n
    }
  }
' "$output"
