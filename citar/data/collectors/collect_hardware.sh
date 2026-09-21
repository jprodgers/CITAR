#!/bin/sh
# CITAR hardware collector for Linux and macOS machines without Python.
#   sh collect_hardware.sh > my-machine.json
# Then import the JSON on CITAR's Servers page. (With Python 3, collect_hardware.py gathers a bit more.)
# Uses only coreutils plus whatever is present: nvidia-smi, rocm-smi, lspci, lsblk, system_profiler.
set -u

esc() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g; s/	/ /g'; }
str() { if [ -z "${1:-}" ]; then printf 'null'; else printf '"%s"' "$(esc "$1")"; fi; }
num() { case "${1:-}" in ''|*[!0-9.]*) printf 'null' ;; *) printf '%s' "$1" ;; esac; }
gb()  { if [ -z "${1:-}" ] || [ "$1" = "0" ]; then printf 'null'; else awk -v b="$1" 'BEGIN{printf "%.1f", b/1073741824}'; fi; }

OS=$(uname -s)
HOST=$(hostname 2>/dev/null || uname -n)
ARCH=$(uname -m)
FORM=""; MANUF=""; MODEL=""; CPU=""; CORES=""; THREADS=""; MHZ=""; RAM_B=""; UNIFIED=false
GPUS=""; DISKS=""; VOLUMES=""; RUNTIMES=""; RELEASE=$(uname -r)

add() { # add "$list" "$item" -> comma separated
  if [ -z "$1" ]; then printf '%s' "$2"; else printf '%s,%s' "$1" "$2"; fi
}

if [ "$OS" = "Linux" ]; then
  [ -r /etc/os-release ] && RELEASE=$(. /etc/os-release 2>/dev/null; printf '%s' "${PRETTY_NAME:-$RELEASE}")
  CPU=$(awk -F': ' '/^model name/{print $2; exit}' /proc/cpuinfo 2>/dev/null)
  [ -z "$CPU" ] && CPU=$(awk -F': ' '/^Model name/{print $2; exit}' /proc/cpuinfo 2>/dev/null)
  THREADS=$(grep -c '^processor' /proc/cpuinfo 2>/dev/null)
  CORES=$(awk -F': ' '/^cpu cores/{print $2; exit}' /proc/cpuinfo 2>/dev/null)
  SOCKETS=$(awk -F': ' '/^physical id/{print $2}' /proc/cpuinfo 2>/dev/null | sort -u | wc -l)
  [ -n "${CORES:-}" ] && [ "${SOCKETS:-1}" -gt 1 ] 2>/dev/null && CORES=$((CORES * SOCKETS))
  MHZ=$(awk -F': ' '/^cpu MHz/{printf "%.2f", $2/1000; exit}' /proc/cpuinfo 2>/dev/null)
  KB=$(awk '/^MemTotal:/{print $2; exit}' /proc/meminfo 2>/dev/null)
  [ -n "${KB:-}" ] && RAM_B=$((KB * 1024))
  MANUF=$(cat /sys/class/dmi/id/sys_vendor 2>/dev/null)
  MODEL=$(cat /sys/class/dmi/id/product_name 2>/dev/null)
  CT=$(cat /sys/class/dmi/id/chassis_type 2>/dev/null)
  case "${CT:-}" in
    8|9|10|14|30|31|32) FORM=laptop ;;
    3|4|5|6|7|13|35) FORM=desktop ;;
    17|23|28|29) FORM=server ;;
  esac
  [ -z "$FORM" ] && ls /sys/class/power_supply 2>/dev/null | grep -q '^BAT' && FORM=laptop
  if command -v lsblk >/dev/null 2>&1; then
    # -P prints KEY="value" pairs, so a model name with spaces (or an empty column) can't shift the fields
    DISKS=$(lsblk -dbnP -o NAME,MODEL,SIZE,ROTA,TRAN 2>/dev/null | awk '
      {
        name = model = size = rota = tran = "";
        if (match($0, /NAME="[^"]*"/)) name = substr($0, RSTART + 6, RLENGTH - 7);
        if (match($0, /MODEL="[^"]*"/)) model = substr($0, RSTART + 7, RLENGTH - 8);
        if (match($0, /SIZE="[^"]*"/)) size = substr($0, RSTART + 6, RLENGTH - 7);
        if (match($0, /ROTA="[^"]*"/)) rota = substr($0, RSTART + 6, RLENGTH - 7);
        if (match($0, /TRAN="[^"]*"/)) tran = substr($0, RSTART + 6, RLENGTH - 7);
        if (name ~ /^(loop|zram|ram|sr)/ || size + 0 == 0) next;
        printf "%s{\"name\":\"%s\",\"type\":\"%s\",\"bus\":\"%s\",\"size_gb\":%.1f}", (n++ ? "," : ""),
               (model ? model : name), (rota == 1 ? "HDD" : "SSD"), toupper(tran), size / 1073741824;
      }')
  fi
elif [ "$OS" = "Darwin" ]; then
  RELEASE=$(sw_vers -productVersion 2>/dev/null)
  CPU=$(sysctl -n machdep.cpu.brand_string 2>/dev/null)
  CORES=$(sysctl -n hw.physicalcpu 2>/dev/null)
  THREADS=$(sysctl -n hw.logicalcpu 2>/dev/null)
  RAM_B=$(sysctl -n hw.memsize 2>/dev/null)
  MANUF=Apple
  MODEL=$(sysctl -n hw.model 2>/dev/null)
  case "$ARCH" in arm64) UNIFIED=true ;; esac
  case "$MODEL" in *Book*) FORM=laptop ;; *) FORM=desktop ;; esac
fi

# --- GPUs
if command -v nvidia-smi >/dev/null 2>&1; then
  GPUS=$(nvidia-smi --query-gpu=name,memory.total,driver_version,power.limit,power.draw \
      --format=csv,noheader,nounits 2>/dev/null | awk -F', *' '
    NF >= 5 {
      lim = ($4 + 0 == $4) ? $4 : "null"; draw = ($5 + 0 == $5) ? $5 : "null";
      printf "%s{\"name\":\"%s\",\"vendor\":\"NVIDIA\",\"vram_gb\":%.1f,\"driver\":\"%s\",\"power_limit_w\":%s,\"power_now_w\":%s,\"power_readable\":true}", (n++ ? "," : ""), $1, $2/1024, $3, lim, draw;
    }')
fi
if [ -z "$GPUS" ] && command -v rocm-smi >/dev/null 2>&1; then
  VRAM=$(rocm-smi --showmeminfo vram 2>/dev/null | awk '/Total Memory/{print $NF; exit}')
  NAME=$(rocm-smi --showproductname 2>/dev/null | awk -F': ' '/Card series|Card Series/{print $2; exit}')
  [ -n "${NAME:-}" ] && GPUS="{\"name\":\"$(esc "$NAME")\",\"vendor\":\"AMD\",\"vram_gb\":$(gb "${VRAM:-0}"),\"power_readable\":true}"
fi
if [ -z "$GPUS" ] && command -v lspci >/dev/null 2>&1; then
  GPUS=$(lspci 2>/dev/null | grep -E "VGA|3D controller|Display controller" | sed 's/.*: //' | awk '
    { gsub(/"/, "");
      vendor = /NVIDIA/ ? "NVIDIA" : (/AMD|ATI/ ? "AMD" : (/Intel/ ? "Intel" : ""));
      printf "%s{\"name\":\"%s\",\"vendor\":\"%s\",\"vram_gb\":null,\"power_readable\":false}", (n++ ? "," : ""), $0, vendor; }')
fi
if [ -z "$GPUS" ] && [ "$OS" = "Darwin" ] && command -v system_profiler >/dev/null 2>&1; then
  NAME=$(system_profiler SPDisplaysDataType 2>/dev/null | awk -F': ' '/Chipset Model/{print $2; exit}')
  [ -n "${NAME:-}" ] && GPUS="{\"name\":\"$(esc "$NAME")\",\"vendor\":\"Apple\",\"vram_gb\":$(gb "${RAM_B:-0}"),\"unified_memory\":true,\"power_readable\":false}"
fi

# --- volumes
VOLUMES=$(df -Pk 2>/dev/null | awk 'NR > 1 && $1 ~ /^\/dev/ {
    printf "%s{\"mount\":\"%s\",\"total_gb\":%.1f,\"free_gb\":%.1f}", (n++ ? "," : ""), $6, $2/1048576, $4/1048576; }')

# --- local model runtimes
LMS=""
for c in "$HOME/.lmstudio/bin/lms" "$(command -v lms 2>/dev/null)"; do
  [ -n "${c:-}" ] && [ -x "$c" ] && LMS="$c" && break
done
if [ -n "$LMS" ]; then
  MODELS=$("$LMS" ls 2>/dev/null | awk 'NF >= 4 && $1 !~ /^(LLM|EMBEDDING|You|$)/ {
      printf "%s{\"key\":\"%s\"}", (n++ ? "," : ""), $1; }')
  RUNTIMES="{\"name\":\"LM Studio\",\"cli\":\"$(esc "$LMS")\",\"models\":[${MODELS:-}]}"
fi
if command -v ollama >/dev/null 2>&1; then
  MODELS=$(ollama list 2>/dev/null | awk 'NR > 1 && NF { printf "%s{\"key\":\"%s\"}", (n++ ? "," : ""), $1; }')
  RUNTIMES=$(add "$RUNTIMES" "{\"name\":\"Ollama\",\"cli\":\"$(command -v ollama)\",\"models\":[${MODELS:-}]}")
fi

# --- a first guess at power (replace with measured numbers when you can)
IDLE=55; CPUMAX=125
[ "${FORM:-}" = "laptop" ] && IDLE=12 && CPUMAX=35
case "$CPU" in *Threadripper*|*Xeon*|*EPYC*) IDLE=110; CPUMAX=280 ;; esac
GPUMAX=$(printf '%s' "${GPUS:-}" | awk '{
    total = 0;
    while (match($0, /"power_limit_w":[0-9.]+/)) {          # 16 characters of key, then the number
      total += 0.85 * substr($0, RSTART + 16, RLENGTH - 16);
      $0 = substr($0, RSTART + RLENGTH);
    }
    if (total == 0) total = gsub(/"vendor":"(NVIDIA|AMD)"/, "&") * 220;
    printf "%d", total; }')
[ -z "${GPUMAX:-}" ] && GPUMAX=0

cat <<JSON
{
  "format": "citar-hardware",
  "collector_version": 1,
  "collector": "shell",
  "collected_at": "$(date +%Y-%m-%dT%H:%M:%S)",
  "hostname": $(str "$HOST"),
  "os": {"system": $(str "$OS"), "release": $(str "$RELEASE"), "arch": $(str "$ARCH")},
  "system": {"form": $(str "${FORM:-}"), "manufacturer": $(str "${MANUF:-}"), "model": $(str "${MODEL:-}")},
  "cpu": {"model": $(str "${CPU:-}"), "cores": $(num "${CORES:-}"), "threads": $(num "${THREADS:-}"), "max_ghz": $(num "${MHZ:-}")},
  "memory": {"ram_gb": $(gb "${RAM_B:-0}"), "unified": ${UNIFIED}},
  "gpus": [${GPUS:-}],
  "physical_disks": [${DISKS:-}],
  "volumes": [${VOLUMES:-}],
  "runtimes": [${RUNTIMES:-}],
  "power": {"gpu_sampling": $(command -v nvidia-smi >/dev/null 2>&1 && printf 'true' || printf 'false'),
    "cpu_rapl": $([ -d /sys/class/powercap/intel-rapl ] && printf 'true' || printf 'false'),
    "suggested": {"idle_w": ${IDLE}, "cpu_max_w": ${CPUMAX}, "gpu_max_w": ${GPUMAX},
      "source": "guess",
      "note": "Estimated from the hardware class; measure idle and busy wall power with a plug-in meter for better numbers."}}
}
JSON
