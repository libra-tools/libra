#!/usr/bin/env python3
"""Stop unfinished Actions for older main commits after a new main push."""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass

ACTIVE_STATUSES = ("requested", "waiting", "pending", "queued", "in_progress")
QUIET_POLL_SECONDS = 10
ACTIVE_POLL_SECONDS = 30
QUIET_SECONDS = 20
RETRY_CANCEL_SECONDS = 30
FORCE_AFTER_SECONDS = 120
MAX_SECONDS = 300
SHA_RE = re.compile(r"[0-9a-f]{40}\Z")
REPO_RE = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z")


class ApiError(RuntimeError):
    pass


class RateLimitError(ApiError):
    pass


class UnqueuedConflictError(ApiError):
    pass


class Superseded(RuntimeError):
    pass


@dataclass(frozen=True)
class Context:
    repo: str
    expected_sha: str
    own_run_id: int


@dataclass
class Attempt:
    first_accepted_at: float | None = None
    last_normal_at: float | None = None
    last_force_at: float | None = None


@dataclass
class Scan:
    runs: list[dict]
    overflow: bool


class GitHub:
    def __init__(self, repo: str, clock=None, pause=None):
        self.base = f"repos/{repo}"
        self.clock = clock or time.monotonic
        self.pause = pause or time.sleep
        self.deadline: float | None = None

    def api(self, method: str, path: str) -> object:
        # Arguments are passed without a shell. Never log the token, raw JSON,
        # or stderr: GitHub may include sensitive details in an error body.
        command = ["gh", "api", "--method", method, path]
        for retry in range(3):
            remaining = None if self.deadline is None else self.deadline - self.clock()
            if remaining is not None and remaining <= 0:
                raise ApiError("cleanup deadline exhausted before API call")
            timeout = min(30, remaining) if remaining is not None else 30
            try:
                process = subprocess.run(
                    command, capture_output=True, text=True, check=False, timeout=timeout
                )
            except subprocess.TimeoutExpired as error:
                if retry == 2:
                    raise ApiError(f"{method} {path}: request timed out") from error
                delay = 5 * (retry + 1)
                if self.deadline is not None and self.clock() + delay >= self.deadline:
                    raise ApiError("cleanup deadline exhausted during API timeout retry") from error
                self.pause(delay)
                continue
            if process.returncode == 0:
                if method == "POST":
                    return None
                try:
                    return json.loads(process.stdout)
                except json.JSONDecodeError as error:
                    raise ApiError(f"{method} {path}: invalid JSON") from error
            stderr = process.stderr.lower()
            if method == "POST" and "http 409" in stderr:
                if "not been queued yet" in stderr:
                    raise UnqueuedConflictError("cancel conflict: HTTP 409 (not queued yet)")
                raise ApiError("cancel conflict: HTTP 409")
            transient = any(f"http {code}" in stderr for code in (429, 500, 502, 503, 504))
            transient = transient or ("http 403" in stderr and "rate limit" in stderr)
            rate_limited = "http 429" in stderr or ("http 403" in stderr and "rate limit" in stderr)
            if transient and retry < 2:
                delay = (60 if rate_limited else 5) * (retry + 1)
                if self.deadline is not None and self.clock() + delay >= self.deadline:
                    if rate_limited:
                        raise RateLimitError("rate limit extends past cleanup deadline")
                    raise ApiError("API retry extends past cleanup deadline")
                self.pause(delay)
                continue
            if rate_limited:
                raise RateLimitError(f"{method} {path}: rate limit persists after retries")
            raise ApiError(f"{method} {path}: gh API exit {process.returncode}")
        raise ApiError(f"{method} {path}: retries exhausted")

    def main_sha(self) -> str:
        result = self.api("GET", f"{self.base}/branches/main")
        return result["commit"]["sha"]

    def compare(self, candidate_sha: str, expected_sha: str) -> str:
        result = self.api("GET", f"{self.base}/compare/{candidate_sha}...{expected_sha}")
        return result["status"]

    def run(self, run_id: int) -> dict:
        return self.api("GET", f"{self.base}/actions/runs/{run_id}")

    def has_jobs(self, run_id: int) -> bool:
        data = self.api("GET", f"{self.base}/actions/runs/{run_id}/jobs?per_page=1")
        return bool(data.get("total_count", 0))

    def cancel(self, run_id: int, force: bool = False) -> None:
        suffix = "force-cancel" if force else "cancel"
        self.api("POST", f"{self.base}/actions/runs/{run_id}/{suffix}")

    def active_runs(self) -> Scan:
        runs: dict[int, dict] = {}
        overflow = False
        for status in ACTIVE_STATUSES:
            for page in range(1, 11):
                path = (
                    f"{self.base}/actions/runs?branch=main&status={status}"
                    f"&per_page=100&page={page}"
                )
                data = self.api("GET", path)
                overflow |= data["total_count"] > 1000
                batch = data["workflow_runs"]
                for run in batch:
                    runs[run["id"]] = run
                if len(batch) < 100:
                    break
            # Search results beyond 1,000 are inaccessible through this query.
            # Process the visible candidates, then report overflow as failure.
        return Scan(list(runs.values()), overflow)


def eligible(run: dict, context: Context) -> bool:
    if run.get("status") not in ACTIVE_STATUSES or run.get("id") == context.own_run_id:
        return False
    if run.get("head_branch") != "main" or run.get("head_sha") == context.expected_sha:
        return False
    if not SHA_RE.fullmatch(run.get("head_sha") or ""):
        return False
    if run.get("event") in ("pull_request", "pull_request_target", "merge_group"):
        return False
    path = run.get("path") or ""
    if not path or "@refs/tags/" in path or path.split("@")[0].endswith("/release.yml"):
        return False
    return True


def check_head(client: GitHub, context: Context, pause=time.sleep) -> None:
    for attempt in range(3):
        observed_sha = client.main_sha()
        if observed_sha == context.expected_sha:
            return
        if not SHA_RE.fullmatch(observed_sha):
            raise ApiError("main branch returned an invalid commit SHA")
        relation = client.compare(context.expected_sha, observed_sha)
        if relation == "ahead":
            raise Superseded("a newer main push owns cleanup")
        # A branch replica can return the previous commit just after this push.
        # Do not pretend a newer cleaner exists when the read is merely stale.
        if relation == "behind" and attempt < 2:
            pause(2)
            continue
        raise ApiError(f"main branch SHA mismatch with {relation} ancestry")


def candidate_ids(
    client: GitHub, context: Context, scan: Scan, relation_cache: dict[str, str]
) -> tuple[list[int], dict[int, str]]:
    selected = []
    errors = {}
    for run in scan.runs:
        # An old main run with missing API identity must not be silently
        # omitted from a clean verdict. Exclude PR and release before this.
        if (
            run.get("status") in ACTIVE_STATUSES
            and run.get("head_branch") == "main"
            and run.get("head_sha") != context.expected_sha
            and run.get("event") not in ("pull_request", "pull_request_target", "merge_group")
            and run.get("id") != context.own_run_id
            and (not SHA_RE.fullmatch(run.get("head_sha") or "") or not run.get("path"))
        ):
            errors[run["id"]] = "old main run has incomplete SHA or workflow path"
            continue
        if not eligible(run, context):
            continue
        run_id = run["id"]
        try:
            candidate_sha = run["head_sha"]
            if candidate_sha not in relation_cache:
                relation_cache[candidate_sha] = client.compare(candidate_sha, context.expected_sha)
            relation = relation_cache[candidate_sha]
            if relation == "ahead":
                selected.append(run_id)
            elif relation == "diverged":
                errors[run_id] = "commit ancestry diverged; manual review required"
            # `behind` is a newer commit's run, even if a branch read was stale.
        except RateLimitError:
            raise
        except ApiError as error:
            errors[run_id] = str(error)
    return selected, errors


def cleanup(client: GitHub, context: Context, clock=time.monotonic, pause=time.sleep) -> dict:
    start = clock()
    deadline = start + MAX_SECONDS
    if hasattr(client, "deadline"):
        client.deadline = deadline
    last_activity = start
    clean_scans = 0
    attempts: dict[int, Attempt] = {}
    requested: set[int] = set()
    forced: set[int] = set()
    skipped: set[int] = set()
    relation_cache: dict[str, str] = {}
    last_remaining: list[int] = []
    last_errors: dict[int, str] = {}
    last_overflow = False

    while clock() < deadline:
        check_head(client, context, pause)
        scan = client.active_runs()
        candidates, errors = candidate_ids(client, context, scan, relation_cache)
        candidates = [run_id for run_id in candidates if run_id not in skipped]
        now = clock()
        for run_id in candidates:
            attempt = attempts.setdefault(run_id, Attempt())
            force = (
                attempt.first_accepted_at is not None
                and now - attempt.first_accepted_at >= FORCE_AFTER_SECONDS
            )
            if force:
                if attempt.last_force_at is not None and now - attempt.last_force_at < RETRY_CANCEL_SECONDS:
                    continue
            elif attempt.last_normal_at is not None and now - attempt.last_normal_at < RETRY_CANCEL_SECONDS:
                continue
            try:
                check_head(client, context, pause)
                client.cancel(run_id, force=force)
                if force:
                    attempt.last_force_at = now
                    forced.add(run_id)
                    print(f"force_cancel_requested run_id={run_id}")
                    if clock() + 1 < deadline:
                        pause(1)  # Space mutation calls to avoid secondary rate limits.
                else:
                    attempt.last_normal_at = now
                    if attempt.first_accepted_at is None:
                        attempt.first_accepted_at = now
                    requested.add(run_id)
                    print(f"cancel_requested run_id={run_id}")
                    if clock() + 1 < deadline:
                        pause(1)  # Space mutation calls to avoid secondary rate limits.
            except RateLimitError:
                raise
            except ApiError as error:
                try:
                    run_info = client.run(run_id)
                    finished = run_info.get("status") == "completed"
                except RateLimitError:
                    raise
                except ApiError as recheck_error:
                    errors[run_id] = f"cancel and status recheck failed: {recheck_error}"
                    continue
                if not finished:
                    if (
                        force
                        and isinstance(error, UnqueuedConflictError)
                        and run_info.get("status") == "queued"
                    ):
                        try:
                            has_jobs = client.has_jobs(run_id)
                        except RateLimitError:
                            raise
                        except ApiError:
                            has_jobs = True
                        if not has_jobs:
                            skipped.add(run_id)
                            errors.pop(run_id, None)
                            print(f"skipped_zombie_run run_id={run_id} reason=unqueued_409")
                            continue
                    errors[run_id] = str(error)
                    # GitHub can return 409 while a prior cancellation is in
                    # progress. Start the same bounded escalation clock.
                    if "HTTP 409" in str(error) and attempt.first_accepted_at is None:
                        attempt.first_accepted_at = now
                        attempt.last_normal_at = now
        # Keep the last observed state: the API budget must not be used for a
        # new scan after its deadline has expired.
        remaining = [run_id for run_id in candidates if run_id not in skipped]
        last_remaining = remaining
        last_errors = errors
        last_overflow = scan.overflow
        if remaining or errors or scan.overflow:
            last_activity = clock()
            clean_scans = 0
        else:
            clean_scans += 1
            if clean_scans >= 2 and clock() - start >= QUIET_SECONDS and clock() - last_activity >= QUIET_SECONDS:
                return {
                    "requested": sorted(requested),
                    "forced": sorted(forced),
                    "skipped": sorted(skipped),
                    "remaining": [],
                    "errors": {},
                    "overflow": False,
                    "settled": True,
                }
        interval = ACTIVE_POLL_SECONDS if remaining or errors or scan.overflow else QUIET_POLL_SECONDS
        pause(min(interval, max(0, deadline - clock())))

    return {
        "requested": sorted(requested),
        "forced": sorted(forced),
        "skipped": sorted(skipped),
        "remaining": sorted(last_remaining),
        "errors": last_errors,
        "overflow": last_overflow,
        "settled": False,
    }


def main() -> int:
    if os.environ.get("GITHUB_EVENT_NAME") != "push" or os.environ.get("GITHUB_REF") != "refs/heads/main":
        raise RuntimeError("cleanup may run only for a main push")
    repo = os.environ.get("GITHUB_REPOSITORY", "")
    expected_sha = os.environ.get("GITHUB_SHA", "")
    run_id = os.environ.get("GITHUB_RUN_ID", "")
    if not REPO_RE.fullmatch(repo) or not SHA_RE.fullmatch(expected_sha) or not run_id.isdecimal():
        raise RuntimeError("missing or invalid GitHub main push context")
    context = Context(repo, expected_sha, int(run_id))
    try:
        result = cleanup(GitHub(repo), context)
    except Superseded:
        print("superseded by a newer main push")
        return 0
    except ApiError as error:
        print(json.dumps({"status": "error", "reason": str(error)}, sort_keys=True))
        return 1
    print(json.dumps(result, sort_keys=True))
    if result["remaining"] or result["errors"] or result["overflow"] or not result["settled"]:
        print("old main runs remain active or scan is incomplete", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
