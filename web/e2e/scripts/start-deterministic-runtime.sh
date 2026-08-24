#!/usr/bin/env bash
# W3-15 — start a deterministic `libra code` (default Web) runtime for Playwright.
# W5-07 removed the `--web-only` alias; the flagless default is the Web launch.
#
# Requires a build with `--features test-provider` and LIBRA_ENABLE_TEST_PROVIDER=1.
# Does not run Playwright; leave this process in the foreground, then in another
# shell:
#
#   export LIBRA_E2E_BASE_URL="http://127.0.0.1:${LIBRA_E2E_PORT:-4410}"
#   export LIBRA_E2E_BOOTSTRAP_TOKEN="<token printed as LIBRA_E2E_BOOTSTRAP_TOKEN=…>"
#   export LIBRA_E2E_REQUIRE=1
#   pnpm --dir web test:e2e
#
# Cleanup: Ctrl-C this script (or kill the recorded PID). Playwright artifacts
# live under web/test-results/ and web/playwright-report/ — remove those dirs on
# failure triage; they are gitignored.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
PORT="${LIBRA_E2E_PORT:-4410}"
FIXTURE="${LIBRA_E2E_FIXTURE:-$ROOT/web/e2e/fixtures/e2e_main_chain.json}"
AUTO_WORKDIR=0
if [[ -n "${LIBRA_E2E_WORKDIR:-}" ]]; then
  WORKDIR="${LIBRA_E2E_WORKDIR}"
else
  WORKDIR="$(mktemp -d -t libra-e2e-XXXXXX)"
  AUTO_WORKDIR=1
fi
CONTROL_DIR="${WORKDIR}/.libra-e2e-control"
mkdir -p "${CONTROL_DIR}"

cleanup() {
  if [[ -n "${RUNTIME_PID:-}" ]] && kill -0 "${RUNTIME_PID}" 2>/dev/null; then
    kill "${RUNTIME_PID}" 2>/dev/null || true
    wait "${RUNTIME_PID}" 2>/dev/null || true
  fi
  if [[ "${AUTO_WORKDIR}" -eq 1 && -d "${WORKDIR}" ]]; then
    rm -rf "${WORKDIR}"
  fi
}
trap cleanup EXIT INT TERM

echo "e2e workdir: ${WORKDIR}"
echo "e2e fixture: ${FIXTURE}"
echo "e2e port:    ${PORT}"

export LIBRA_ENABLE_TEST_PROVIDER=1
export LIBRA_SKIP_WEB_BUILD="${LIBRA_SKIP_WEB_BUILD:-1}"
# Short lease so a crashed Playwright worker does not block the next local run.
export LIBRA_CODE_LEASE_DURATION_MS="${LIBRA_CODE_LEASE_DURATION_MS:-15000}"

resolve_bin() {
  if [[ -n "${LIBRA_E2E_BIN:-}" ]]; then
    if [[ ! -x "${LIBRA_E2E_BIN}" ]]; then
      echo "LIBRA_E2E_BIN=${LIBRA_E2E_BIN} is not executable" >&2
      exit 1
    fi
    # Explicit override must already include test-provider; callers own that.
    echo "${LIBRA_E2E_BIN}"
    return
  fi
  echo "Building libra with --features test-provider (LIBRA_SKIP_WEB_BUILD=${LIBRA_SKIP_WEB_BUILD})…" >&2
  (cd "${ROOT}" && cargo build --features test-provider)
  local built="${ROOT}/target/debug/libra"
  if [[ ! -x "${built}" ]]; then
    echo "cargo build --features test-provider did not produce ${built}" >&2
    exit 1
  fi
  echo "${built}"
}

BIN="$(resolve_bin)"
echo "Using binary: ${BIN}"

(
  cd "${WORKDIR}"
  if [[ ! -d .libra ]]; then
    "${BIN}" init >/dev/null
  fi
)

echo "Starting ${BIN} on http://127.0.0.1:${PORT} …"
RUNTIME_LOG="${CONTROL_DIR}/runtime.log"
(
  cd "${WORKDIR}"
  exec "${BIN}" code \
    --browser-control loopback \
    --control write \
    --context dev \
    --approval-policy on-request \
    --provider fake \
    --model fake-local \
    --fake-fixture "${FIXTURE}" \
    --port "${PORT}" \
    --mcp-port 0 \
    --control-token-file "${CONTROL_DIR}/token" \
    --control-info-file "${CONTROL_DIR}/info.json"
) >"${RUNTIME_LOG}" 2>&1 &
RUNTIME_PID=$!
# Mirror the child log so operators still see the printed Code UI URL.
tail -n +1 -F "${RUNTIME_LOG}" &
TAIL_PID=$!

for _ in $(seq 1 60); do
  if curl -fsS "http://127.0.0.1:${PORT}/api/health" 2>/dev/null | grep -qx 'ok'; then
    TOKEN="$(sed -n 's/^Browser bootstrap token: //p' "${RUNTIME_LOG}" | head -n 1)"
    if [[ -z "${TOKEN}" ]]; then
      echo "runtime became healthy but printed no browser bootstrap token" >&2
      exit 1
    fi
    printf '%s\n' "${TOKEN}" >"${CONTROL_DIR}/bootstrap-token"
    echo "LIBRA_E2E_BASE_URL=http://127.0.0.1:${PORT}"
    echo "LIBRA_E2E_BOOTSTRAP_TOKEN=${TOKEN}"
    echo "Runtime ready (pid ${RUNTIME_PID}). Leave this process running for Playwright."
    echo "Open ${LIBRA_E2E_BASE_URL:-http://127.0.0.1:${PORT}}/?bt=${TOKEN} (loopback write requires the printed token)."
    wait "${RUNTIME_PID}"
    kill "${TAIL_PID}" 2>/dev/null || true
    exit $?
  fi
  if ! kill -0 "${RUNTIME_PID}" 2>/dev/null; then
    echo "runtime exited before health became ready" >&2
    cat "${RUNTIME_LOG}" >&2 || true
    kill "${TAIL_PID}" 2>/dev/null || true
    exit 1
  fi
  sleep 0.5
done

echo "timed out waiting for /api/health on port ${PORT}" >&2
kill "${TAIL_PID}" 2>/dev/null || true
cat "${RUNTIME_LOG}" >&2 || true
exit 1
