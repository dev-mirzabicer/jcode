#!/usr/bin/env bash
# Combined instruction mechanisms and protected integration boundaries.
# Run through `selfdev test`. All instruction fixtures use synthetic prose.
set -u
umask 077
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo" || exit 1
base=${1:-${JCODE_SCRATCH_DIR:-${HOME}/.jcode/scratch}/instruction-integration}
mkdir -p "$base" || exit 1
out=$(mktemp -d "$base/run-XXXXXXXX") || exit 1
export CARGO_INCREMENTAL=0 JCODE_NO_TELEMETRY=1
export JCODE_HOME="$out/home"
mkdir -p "$JCODE_HOME"
printf '[features]\nmemory=false\nswarm=true\n[ambient]\nenabled=false\n[sponsors]\nenabled=false\n' > "$JCODE_HOME/config.toml"
printf 'Artifacts: %s\n' "$out"
git rev-parse HEAD > "$out/source-head.txt"
git diff --binary > "$out/working.patch"
failed=0
run() {
  local label=$1 code
  shift
  printf 'JCODE_PROGRESS {"message":"Instruction integration: %s"}\n' "$label"
  "$@" > "$out/$label.log" 2>&1
  code=$?
  if [[ "$code" == 0 && "$2" == test ]] && ! grep -Eq 'test result: ok\. [1-9][0-9]* passed' "$out/$label.log"; then
    printf 'No passing tests were observed. This is not acceptance evidence.\n' >> "$out/$label.log"
    code=2
  fi
  printf '%s\t%s\n' "$label" "$code" | tee -a "$out/results.tsv"
  grep 'test result:' "$out/$label.log" || true
  if [[ "$code" != 0 ]]; then
    failed=1
    tail -n 24 "$out/$label.log"
    if grep -Eqi 'out of memory|cannot allocate memory|memory allocation.*failed|no space left on device' "$out/$label.log"; then
      printf 'Resource exhaustion requires intervention before continuing. Artifacts: %s\n' "$out"
      exit "$code"
    fi
  fi
}
run base-all scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib -- --test-threads=1
# Fresh processes isolate the known cwd-sensitive aggregate-test boundary.
for family in skill:: model_roster:: prompt::prompt_tests transfer_handoff startup_context; do
  label=${family//:/_}
  run "base-$label" scripts/dev_cargo.sh test --profile selfdev -p jcode-base --lib "$family" -- --test-threads=1
done
run app-core scripts/dev_cargo.sh test --profile selfdev -p jcode-app-core --lib -- --test-threads=1
run domains scripts/dev_cargo.sh test --profile selfdev -p jcode-command-risk -p jcode-context-core -p jcode-instruction-types -p jcode-overnight-core -p jcode-plan -p jcode-protocol -p jcode-session-types -p jcode-swarm-core -p jcode-task-types -p jcode-provider-core -p jcode-provider-claude-cli-runtime --lib -- --test-threads=1
run openrouter scripts/dev_cargo.sh test --profile selfdev -p jcode-provider-openrouter-runtime --lib -- --test-threads=1
run harness scripts/dev_cargo.sh test --profile selfdev -p jcode-harness-api -p jcode-harness-api-server --lib -- --test-threads=1
run sdk scripts/dev_cargo.sh test --profile selfdev -p jcode-sdk -- --test-threads=1
for family in instruction_manager agent_profile skill startup_context context_editor replay kv_cache command remote; do
  run "tui-$family" scripts/dev_cargo.sh test --profile selfdev -p jcode-tui --lib "$family" -- --test-threads=1
done
run tui-messages scripts/dev_cargo.sh test --profile selfdev -p jcode-tui-messages --lib -- --test-threads=1
run root-check scripts/dev_cargo.sh check --profile selfdev -p jcode --all-targets
run strict scripts/dev_cargo.sh clippy --profile selfdev -p jcode -p jcode-app-core -p jcode-base -p jcode-command-risk -p jcode-context-core -p jcode-harness-api -p jcode-harness-api-server -p jcode-instruction-types -p jcode-overnight-core -p jcode-plan -p jcode-protocol -p jcode-provider-claude-cli-runtime -p jcode-provider-core -p jcode-provider-openrouter-runtime -p jcode-sdk -p jcode-session-types -p jcode-swarm-core -p jcode-task-types -p jcode-tui -p jcode-tui-messages --all-targets -- -D warnings
run formatting cargo fmt --all -- --check
run diff git diff --check
printf 'Artifacts: %s\n' "$out"
exit "$failed"
