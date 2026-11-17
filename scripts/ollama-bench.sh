#!/usr/bin/env bash
# One command on the benchmark host: scripts/ollama-bench.sh [MODEL...]
# Needs ollama serving on OLLAMA_URL (default 127.0.0.1:11434) and python3.
# Pulls missing models first; cloud models are refused.
set -euo pipefail
URL="${OLLAMA_URL:-http://127.0.0.1:11434}"
RUNS="${RUNS:-3}"
MODELS=("$@"); [ ${#MODELS[@]} -eq 0 ] && MODELS=(qwen3:4b qwen3:8b)
for m in "${MODELS[@]}"; do
  case "$m" in *cloud*) echo "refusing cloud model $m" >&2; exit 2;; esac
  ollama list | awk '{print $1}' | grep -qx "$m" || ollama pull "$m"
done
lscpu 2>/dev/null | grep -E "Model name|^CPU\(s\)" || sysctl -n machdep.cpu.brand_string 2>/dev/null || true
exec python3 -I "$(cd "$(dirname "$0")" && pwd)/ollama-bench/bench.py" --url "$URL" --runs "$RUNS" "${MODELS[@]}"
