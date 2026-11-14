"""Deterministic safety tests; never uses the network or a model."""

import importlib.util
import io
import json
import subprocess
import sys
import unittest
from pathlib import Path
from contextlib import redirect_stdout
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("cancel_superseded_main_runs.py")
spec = importlib.util.spec_from_file_location("cancel", SCRIPT)
cancel = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = cancel
spec.loader.exec_module(cancel)
A = "a" * 40
B = "b" * 40
C = "c" * 40
CTX = cancel.Context("libra-tools/libra", B, 200)


def run(run_id, sha=A, status="in_progress", event="push", branch="main", path=".github/workflows/base.yml"):
    return {"id": run_id, "head_sha": sha, "status": status, "event": event, "head_branch": branch, "path": path}


class Clock:
    def __init__(self):
        self.value = 0.0
    def now(self):
        return self.value
    def sleep(self, seconds):
        self.value += seconds


class Fake:
    def __init__(self, entries, clock=None, relation=None, fail=None, overflow=False):
        self.entries = list(entries)
        self.clock = clock or Clock()
        self.relation = relation or {A: "ahead", C: "behind"}
        self.fail = set(fail or [])
        self.overflow = overflow
        self.normal = []
        self.forced = []
        self.comparisons = []
        self.head = B
        self.head_reads = 0
        self.change_head_after = None
        self.complete_after = {}
        self.pending_completion = {}

    def main_sha(self):
        self.head_reads += 1
        if self.change_head_after == self.head_reads:
            self.head = C
        return self.head

    def active_runs(self):
        for item in self.entries:
            deadline = self.pending_completion.get(item["id"])
            if deadline is not None and self.clock.now() >= deadline:
                item["status"] = "completed"
        return cancel.Scan([x for x in self.entries if x["status"] != "completed"], self.overflow)

    def compare(self, old, current):
        self.comparisons.append((old, current))
        if old == B and current == C:
            return "ahead"
        if old == B and current == A:
            return "behind"
        assert current == B
        return self.relation[old]

    def cancel(self, run_id, force=False):
        if run_id in self.fail:
            raise cancel.ApiError("cancel rejected")
        if force:
            self.forced.append(run_id)
        else:
            self.normal.append(run_id)
        item = self.run(run_id)
        if force or run_id not in self.complete_after:
            item["status"] = "completed"
        elif self.complete_after[run_id] is not None:
            self.pending_completion[run_id] = self.complete_after[run_id]

    def run(self, run_id):
        return next(x for x in self.entries if x["id"] == run_id)

    def has_jobs(self, run_id):
        return False


def invoke(fake):
    return cancel.cleanup(fake, CTX, clock=fake.clock.now, pause=fake.clock.sleep)


class CleanupTests(unittest.TestCase):
    def test_filters_pr_tag_release_current_and_own_run(self):
        entries = [
            run(1),
            run(2, event="pull_request"),
            run(3, branch="v1.0.0", path=".github/workflows/release.yml@refs/tags/v1.0.0"),
            run(4, path=".github/workflows/release.yml"),
            run(5, sha=B),
            run(200),
            run(6, event="schedule", path=".github/workflows/live-compat.yml"),
        ]
        fake = Fake(entries)
        result = invoke(fake)
        self.assertEqual(fake.normal, [1, 6])
        self.assertEqual(result["remaining"], [])
        self.assertGreaterEqual(fake.clock.now(), cancel.QUIET_SECONDS)

    def test_eligible_rejects_untrusted_or_missing_context(self):
        for candidate in [run(1, event="pull_request_target"), run(2, event="merge_group"), run(3, path=""), run(4, branch="feature"), run(5, sha="invalid"), run(6, status="completed")]:
            self.assertFalse(cancel.eligible(candidate, CTX))

    def test_newer_run_is_not_cancelled_even_with_stale_head_read(self):
        fake = Fake([run(10, sha=C)])
        result = invoke(fake)
        self.assertEqual(result["remaining"], [])
        self.assertEqual(fake.normal, [])

    def test_concurrent_new_push_aborts_before_cancel(self):
        fake = Fake([run(10)])
        fake.change_head_after = 2
        with self.assertRaises(cancel.Superseded):
            invoke(fake)
        self.assertEqual(fake.normal, [])

    def test_stale_branch_read_retries_instead_of_claiming_superseded(self):
        class Stale(Fake):
            def main_sha(self):
                self.head_reads += 1
                return A if self.head_reads == 1 else B
        fake = Stale([run(10)])
        result = invoke(fake)
        self.assertEqual(result["remaining"], [])
        self.assertEqual(fake.normal, [10])

    def test_persistently_stale_branch_read_fails_visible(self):
        fake = Fake([run(10)])
        fake.head = A
        with self.assertRaises(cancel.ApiError):
            invoke(fake)
        self.assertEqual(fake.normal, [])

    def test_empty_scan_waits_for_late_created_run(self):
        class Late(Fake):
            def active_runs(self):
                if self.clock.now() >= 10 and not self.entries:
                    self.entries.append(run(10))
                return super().active_runs()
        fake = Late([])
        result = invoke(fake)
        self.assertEqual(fake.normal, [10])
        self.assertEqual(result["remaining"], [])
        self.assertGreaterEqual(fake.clock.now(), 30)

    def test_async_cancel_waits_without_false_failure(self):
        fake = Fake([run(10)])
        fake.complete_after[10] = 80
        result = invoke(fake)
        self.assertEqual(result["remaining"], [])
        self.assertEqual(fake.forced, [])
        self.assertEqual(fake.comparisons.count((A, B)), 1)
        self.assertLessEqual(fake.head_reads, 10)
        self.assertGreaterEqual(fake.clock.now(), 95)

    def test_accepted_but_lost_cancel_retries_then_forces(self):
        fake = Fake([run(10)])
        fake.complete_after[10] = None
        result = invoke(fake)
        self.assertEqual(result["remaining"], [])
        self.assertGreaterEqual(len(fake.normal), 2)
        self.assertEqual(fake.forced, [10])
        self.assertIn(10, result["forced"])

    def test_single_cancel_failure_does_not_abort_other_candidates(self):
        fake = Fake([run(10), run(11)], fail={10})
        with patch.object(cancel, "MAX_SECONDS", 12):
            result = invoke(fake)
        self.assertEqual(fake.normal, [11])
        self.assertIn(10, result["remaining"])
        self.assertFalse(result["settled"])

    def test_completion_race_409_is_benign(self):
        class FinishedDuringCancel(Fake):
            def cancel(self, run_id, force=False):
                self.run(run_id)["status"] = "completed"
                raise cancel.ApiError("cancel conflict: HTTP 409")
        fake = FinishedDuringCancel([run(10)])
        result = invoke(fake)
        self.assertEqual(result["remaining"], [])
        self.assertEqual(result["errors"], {})

    def test_persistent_409_escalates_to_force(self):
        class Conflict(Fake):
            def cancel(self, run_id, force=False):
                if force:
                    return super().cancel(run_id, force=True)
                raise cancel.ApiError("cancel conflict: HTTP 409")
        fake = Conflict([run(10)])
        result = invoke(fake)
        self.assertEqual(fake.forced, [10])
        self.assertEqual(result["remaining"], [])

    def test_incomplete_old_run_metadata_fails_visible(self):
        fake = Fake([run(10, path="")])
        with patch.object(cancel, "MAX_SECONDS", 12):
            result = invoke(fake)
        self.assertEqual(fake.normal, [])
        self.assertIn(10, result["errors"])

    def test_diverged_relation_is_reported_without_cancel(self):
        fake = Fake([run(10)], relation={A: "diverged"})
        with patch.object(cancel, "MAX_SECONDS", 12):
            result = invoke(fake)
        self.assertEqual(fake.normal, [])
        self.assertIn(10, result["errors"])

    def test_overflow_still_cancels_visible_run_then_reports_failure(self):
        fake = Fake([run(10)], overflow=True)
        with patch.object(cancel, "MAX_SECONDS", 12):
            result = invoke(fake)
        self.assertEqual(fake.normal, [10])
        self.assertTrue(result["overflow"])

    def test_pagination_over_one_hundred(self):
        class Pages(cancel.GitHub):
            def __init__(self):
                super().__init__("libra-tools/libra")
                self.requests = []
            def api(self, method, path):
                self.requests.append(path)
                if "status=queued" in path:
                    if path.endswith("&page=1"):
                        return {"total_count": 101, "workflow_runs": [run(i) for i in range(1, 101)]}
                    return {"total_count": 101, "workflow_runs": [run(101)]}
                return {"total_count": 0, "workflow_runs": []}
        api = Pages()
        self.assertEqual(len(api.active_runs().runs), 101)
        self.assertTrue(any("page=2" in p for p in api.requests))

    def test_api_cap_returns_visible_runs_and_overflow(self):
        class Cap(cancel.GitHub):
            def __init__(self):
                super().__init__("libra-tools/libra")
            def api(self, method, path):
                if "status=queued" in path and path.endswith("&page=1"):
                    return {"total_count": 1001, "workflow_runs": [run(i) for i in range(1, 101)]}
                return {"total_count": 1001, "workflow_runs": []}
        scan = Cap().active_runs()
        self.assertEqual(len(scan.runs), 100)
        self.assertTrue(scan.overflow)

    def test_api_retries_secondary_403_rate_limit(self):
        bad = subprocess.CompletedProcess([], 1, "", "HTTP 403: secondary rate limit")
        good = subprocess.CompletedProcess([], 0, json.dumps({"ok": True}), "")
        with patch.object(cancel.subprocess, "run", side_effect=[bad, good]) as call, patch.object(cancel.time, "sleep") as sleep:
            self.assertEqual(cancel.GitHub("libra-tools/libra").api("GET", "repos/libra-tools/libra/branches/main"), {"ok": True})
        self.assertEqual(call.call_count, 2)
        sleep.assert_called_once_with(60)

    def test_api_timeout_retries_then_fails(self):
        error = subprocess.TimeoutExpired(["gh"], 30)
        with patch.object(cancel.subprocess, "run", side_effect=error), patch.object(cancel.time, "sleep") as sleep:
            with self.assertRaises(cancel.ApiError):
                cancel.GitHub("libra-tools/libra").api("GET", "repos/libra-tools/libra/branches/main")
        self.assertEqual(sleep.call_count, 2)

    def test_deadline_reports_last_observed_run_with_real_client_subclass(self):
        clock = Clock()
        class Stuck(cancel.GitHub):
            def __init__(self):
                super().__init__("libra-tools/libra", clock=clock.now, pause=clock.sleep)
                self.normal = []
                self.forced = []
            def main_sha(self):
                return B
            def active_runs(self):
                return cancel.Scan([run(10)], False)
            def compare(self, old, current):
                return "ahead"
            def cancel(self, run_id, force=False):
                (self.forced if force else self.normal).append(run_id)
            def api(self, method, path):
                raise AssertionError("no API call is allowed after deadline")
        client = Stuck()
        result = cancel.cleanup(client, CTX, clock=clock.now, pause=clock.sleep)
        self.assertEqual(result["remaining"], [10])
        self.assertFalse(result["settled"])
        self.assertEqual(clock.now(), cancel.MAX_SECONDS)
        self.assertEqual(client.deadline, cancel.MAX_SECONDS)
        self.assertTrue(client.forced)

    def test_rate_limit_backoff_respects_cleanup_deadline(self):
        clock = Clock()
        api = cancel.GitHub("libra-tools/libra", clock=clock.now, pause=clock.sleep)
        api.deadline = 100
        rate_limited = subprocess.CompletedProcess([], 1, "", "HTTP 429: rate limit")
        with patch.object(cancel.subprocess, "run", return_value=rate_limited) as call:
            with self.assertRaises(cancel.RateLimitError):
                api.api("GET", "repos/libra-tools/libra/branches/main")
        self.assertEqual(call.call_count, 2)
        self.assertEqual(clock.now(), 60)
        self.assertLess(clock.now(), api.deadline)

    def test_rate_limit_during_compare_aborts_optional_candidates(self):
        class Limited(Fake):
            def compare(self, old, current):
                raise cancel.RateLimitError("rate limit")
        fake = Limited([run(10), run(11)])
        with self.assertRaises(cancel.RateLimitError):
            invoke(fake)
        self.assertEqual(fake.normal, [])

    def test_main_api_error_has_structured_failure(self):
        env = {"GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/heads/main", "GITHUB_REPOSITORY": "libra-tools/libra", "GITHUB_SHA": B, "GITHUB_RUN_ID": "200"}
        output = io.StringIO()
        with patch.dict(cancel.os.environ, env), patch.object(cancel, "cleanup", side_effect=cancel.RateLimitError("rate limited")), redirect_stdout(output):
            self.assertEqual(cancel.main(), 1)
        self.assertEqual(json.loads(output.getvalue())["status"], "error")

    def test_api_409_and_post_success(self):
        conflict = subprocess.CompletedProcess([], 1, "", "HTTP 409: Conflict")
        accepted = subprocess.CompletedProcess([], 0, "", "")
        with patch.object(cancel.subprocess, "run", return_value=conflict):
            with self.assertRaisesRegex(cancel.ApiError, "HTTP 409"):
                cancel.GitHub("libra-tools/libra").api("POST", "repos/libra-tools/libra/actions/runs/10/cancel")
        with patch.object(cancel.subprocess, "run", return_value=accepted):
            self.assertIsNone(cancel.GitHub("libra-tools/libra").api("POST", "repos/libra-tools/libra/actions/runs/10/cancel"))

    def test_main_rejects_non_main_or_invalid_context(self):
        with patch.dict(cancel.os.environ, {"GITHUB_EVENT_NAME": "pull_request", "GITHUB_REF": "refs/pull/1/merge"}):
            with self.assertRaisesRegex(RuntimeError, "main push"):
                cancel.main()
        with patch.dict(cancel.os.environ, {"GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/heads/main", "GITHUB_REPOSITORY": "bad", "GITHUB_SHA": B, "GITHUB_RUN_ID": "200"}):
            with self.assertRaisesRegex(RuntimeError, "invalid"):
                cancel.main()

    def test_api_unqueued_409_raises_specific_error(self):
        unqueued = subprocess.CompletedProcess(
            [], 1, "", 'gh: Cannot cancel a workflow run that has not been queued yet. (HTTP 409)'
        )
        with patch.object(cancel.subprocess, "run", return_value=unqueued):
            with self.assertRaisesRegex(cancel.UnqueuedConflictError, "HTTP 409"):
                cancel.GitHub("libra-tools/libra").api("POST", "repos/libra-tools/libra/actions/runs/10/force-cancel")

    def test_unqueued_409_zombie_run_is_skipped_after_force_window(self):
        class Zombie(Fake):
            def cancel(self, run_id, force=False):
                raise cancel.UnqueuedConflictError("cancel conflict: HTTP 409 (not queued yet)")
        fake = Zombie([run(10, status="queued")])
        result = invoke(fake)
        self.assertTrue(result["settled"])
        self.assertEqual(result["skipped"], [10])
        self.assertEqual(result["remaining"], [])
        self.assertEqual(result["errors"], {})
        self.assertGreaterEqual(fake.clock.now(), cancel.FORCE_AFTER_SECONDS)

    def test_unqueued_409_with_jobs_is_not_skipped(self):
        class Busy(Fake):
            def cancel(self, run_id, force=False):
                raise cancel.UnqueuedConflictError("cancel conflict: HTTP 409 (not queued yet)")
            def has_jobs(self, run_id):
                return True
        fake = Busy([run(10, status="queued")])
        result = invoke(fake)
        self.assertFalse(result["settled"])
        self.assertEqual(result["skipped"], [])
        self.assertEqual(result["remaining"], [10])
        self.assertIn(10, result["errors"])


if __name__ == "__main__":
    unittest.main()
