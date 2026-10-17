#!/usr/bin/env bash
# Runs the tests under every stdlib feature combination that matters (ADR 0007): no modules, every
# module, and each module alone. "Each module alone" is the one that finds a dependency put behind
# the wrong feature, or a module that quietly needs another's Rust.
#
# Extra arguments go to `cargo test`, before `--`, e.g. `scripts/check-features.sh --quiet`.
set -euo pipefail
cd "$(dirname "$0")/.."

modules=(http fs crypto serde datetime utils validation dirs process tidings bridge)
# The bundled modules (ADR 0026), each built alone too: its feature has to turn on what it needs.
bundled=(openai)
failed=()

run() {
    local label=$1
    shift
    echo
    echo "==> $label"
    if ! cargo test "$@" "${extra[@]}"; then
        failed+=("$label")
    fi
}

extra=("$@")

# Every module, and the avarice program: what a plain `cargo test` builds.
run "all modules (default features)"
# None: what an embedder gets from `default-features = false` and nothing else.
run "no modules" --no-default-features
for module in "${modules[@]}"; do
    run "only $module" --no-default-features --features "stdlib-$module"
done
for module in "${bundled[@]}"; do
    run "only bundled $module" --no-default-features --features "bundled-$module"
done

echo
if ((${#failed[@]})); then
    echo "FAILED: ${failed[*]}"
    exit 1
fi
singles=$((${#modules[@]} + ${#bundled[@]}))
echo "all $singles single-module builds, the empty build and the full build passed"
