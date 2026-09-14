"""Verification-only MIG-06 artifact; never merge this branch into main.

Real hash-pinned releases, an isolated synthetic fixture and a strict 10s
budget. Local runs are labelled non-CI and cannot close the CI requirement.
"""

import argparse
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import sqlite3
import stat
import subprocess
import sys
import tempfile
import time
import unittest


PINS = {
    "0.22.19": ("03447eb983178433425b5afffba351edae4044e7ded5b9e35c956dddb2bb68a6", 116642608),
    "0.22.26": ("6d6599a2babab22a14bbe0a001f47f98d4205ae13f3e55142b845d0fb8b6ccb1", 116926712),
}
RELEASE_SOURCE = "b60005021afd90bcb720d329b5b776fec4c55e7d"
PAYLOAD_BYTES = 100 * 1024 * 1024
BENCHMARK_SQL = """
WITH RECURSIVE sizes(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM sizes WHERE n<100)
INSERT INTO config_kv(key,value,encrypted)
SELECT 'benchmark.'||n,zeroblob(1048576),0 FROM sizes
"""


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def child_environment(root):
    fixture_home = root / "home"
    return {
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(fixture_home),
        "USERPROFILE": str(fixture_home), "XDG_CONFIG_HOME": str(fixture_home / ".config"),
        "LIBRA_CONFIG_GLOBAL_DB": str(fixture_home / ".libra/config.db"),
        "LIBRA_CONFIG_SYSTEM_DB": str(root / "system.db"), "LIBRA_TEST": "1",
    }


def run_child(args, root, env, label, timeout=60):
    child = subprocess.Popen(
        [str(arg) for arg in args], cwd=root, env=env,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True,
    )
    try:
        stdout, _stderr = child.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        os.killpg(child.pid, signal.SIGTERM)
        try:
            child.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGKILL)
            child.communicate()
        raise RuntimeError(f"{label} exceeded its bounded execution time") from None
    require(child.returncode == 0, f"{label} failed (exit {child.returncode})")
    return stdout


def verify_binary(path, pin):
    digest, size = pin
    require(path.is_file() and not path.is_symlink(), "release cache must be a regular file")
    require(path.stat().st_size == size, "release binary size differs from its pinned artifact")
    with path.open("rb") as binary:
        actual = hashlib.file_digest(binary, "sha256").hexdigest()
    require(actual == digest, "release binary SHA-256 differs from its pinned artifact")


def get_binary(directory, version):
    path = directory / f"libra-v{version}-linux-amd64"
    if not path.exists():
        url = f"https://download.libra.tools/libra/releases/v{version}/libra-linux-amd64"
        # Use the release installer's supported curl client. Do not spoof a
        # user agent, follow redirects, load curlrc/netrc, or inherit credentials.
        run_child([
            "/usr/bin/curl", "--disable", "--fail", "--silent", "--show-error",
            "--proto", "=https", "--connect-timeout", "30", "--max-time", "180",
            "--max-filesize", str(PINS[version][1]), "--output", path, url,
        ], directory, {"PATH": "/usr/bin:/bin", "HOME": str(directory)},
            f"pinned v{version} release download", timeout=190)
    verify_binary(path, PINS[version])
    path.chmod(0o700)
    return path.resolve()


def readonly(path):
    return closing(sqlite3.connect(path.as_uri() + "?mode=ro", uri=True))


def snapshot(connection):
    return {
        "receipts": connection.execute("SELECT * FROM schema_versions ORDER BY version").fetchall(),
        "legacy": connection.execute("SELECT * FROM config ORDER BY id").fetchall(),
        "sequence": connection.execute("SELECT * FROM sqlite_sequence ORDER BY name").fetchall(),
        "kv_metadata": connection.execute(
            "SELECT id,key,typeof(value),length(value),encrypted FROM config_kv ORDER BY id"
        ).fetchall(),
    }


def verify_payload(connection):
    actual = connection.execute(
        "SELECT count(*),sum(length(value)),sum(value=zeroblob(1048576)) "
        "FROM config_kv WHERE key LIKE 'benchmark.%'"
    ).fetchone()
    require(actual == (100, PAYLOAD_BYTES, 100), "100 MiB benchmark payload changed")
    require(connection.execute(
        "SELECT value,encrypted FROM config_kv WHERE key='test.repair'"
    ).fetchone() == ("preserved", 0), "ordinary synthetic configuration changed")


def benchmark(args, metrics):
    require(platform.system() == "Linux" and platform.machine() == "x86_64", "Linux amd64 runner required")
    is_ci = os.environ.get("GITHUB_ACTIONS") == "true"
    require(is_ci or args.allow_local, "CI evidence requires Actions; label diagnostics with --allow-local")
    metrics["ci"] = is_ci
    metrics["runner"] = {
        "os": platform.platform(), "arch": platform.machine(), "cpu_count": os.cpu_count(),
        "image_os": os.environ.get("ImageOS", "local"),
        "image_version": os.environ.get("ImageVersion", "local"),
    }
    with tempfile.TemporaryDirectory(prefix="mig06-benchmark-", dir=os.environ.get("RUNNER_TEMP")) as temporary:
        root = Path(temporary).resolve()
        directory = args.binaries_dir.resolve() if args.binaries_dir else root / "binaries"
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        metrics["stage"] = "verify_binaries"
        print("Verifying pinned v0.22.19 producer and v0.22.26 release", flush=True)
        producer, release = (get_binary(directory, version) for version in PINS)
        env = child_environment(root)
        (root / "home/.libra").mkdir(mode=0o700, parents=True)
        (root / "home/.config").mkdir(mode=0o700)
        sentinel = root / "system.db"
        sentinel.write_bytes(b"synthetic system sentinel: must stay unchanged")
        sentinel_before = hashlib.sha256(sentinel.read_bytes()).hexdigest()
        db = Path(env["LIBRA_CONFIG_GLOBAL_DB"])
        for binary, version in ((producer, "0.22.19"), (release, "0.22.26")):
            require(run_child([binary, "--version"], root, env, "version check").strip()
                    == f"libra {version}".encode(), "release version output differs from its pin")
        metrics["stage"] = "create_synthetic_cohort"
        run_child([producer, "config", "set", "--global", "test.repair", "preserved"], root, env, "real producer")
        with closing(sqlite3.connect(db)) as connection:
            connection.execute(BENCHMARK_SQL)
            connection.commit()
        with readonly(db) as connection:
            before = snapshot(connection)
            require(len(before["receipts"]) == 60, "producer must have exactly 60 receipts")
            verify_payload(connection)
        metrics["fixture_bytes"] = db.stat().st_size
        require(metrics["fixture_bytes"] >= PAYLOAD_BYTES, "fixture is smaller than 100 MiB")
        metrics["runner"]["filesystem"] = run_child(
            ["/usr/bin/stat", "-f", "-c", "%T", db], root, env, "filesystem identification"
        ).decode().strip()
        metrics["stage"] = "timed_repair"
        print("Measuring 100 MiB backup + reopen + repair (budget: 10 seconds)", flush=True)
        started = time.perf_counter()
        output = run_child(
            [release, "--json", "config", "doctor", "--global-schema", "--repair", "--confirm", db],
            root, env, "released repair", timeout=30,
        )
        metrics["elapsed_seconds"] = time.perf_counter() - started
        report = json.loads(output)["data"]
        require(report["action"] == "repair" and report["outcome"] == "repaired", "repair did not commit")
        require(report["backup_verified"] is True and report["committed"] is True, "backup/commit evidence missing")
        require(report["producer_binary_sha256"] == PINS["0.22.19"][0], "producer attestation differs from pin")
        backup = Path(report["backup_path"])
        require(backup.resolve().is_relative_to(db.parent), "backup escaped isolated fixture")
        require(not backup.is_symlink() and stat.S_IMODE(backup.stat().st_mode) == 0o600, "backup is not private")
        require(stat.S_IMODE(backup.parent.stat().st_mode) == 0o700, "recovery directory is not private")
        recovery = json.loads((backup.parent / "recovery.json").read_text())
        require(recovery == report, "retained recovery report differs from committed CLI report")
        with readonly(backup) as connection:
            require(connection.execute("PRAGMA quick_check").fetchone() == ("ok",), "backup quick_check failed")
            require(snapshot(connection) == before, "backup row metadata or receipt set changed")
            verify_payload(connection)
        with readonly(db) as connection:
            after = snapshot(connection)
            barrier = connection.execute("SELECT * FROM schema_versions WHERE version=9223372036854775807").fetchone()
            require(after.pop("receipts") == before["receipts"] + [barrier], "legacy receipts changed beyond barrier")
            require(after == {key: value for key, value in before.items() if key != "receipts"}, "configuration changed")
            require(connection.execute("SELECT version,name FROM configuration_schema_versions").fetchall()
                    == [(2026090601, "configuration_base")], "configuration ledger is not minimal")
            require(barrier is not None and barrier[1] == "configuration_legacy_reader_barrier", "barrier missing")
            verify_payload(connection)
        require(hashlib.sha256(sentinel.read_bytes()).hexdigest() == sentinel_before, "system sentinel changed")
        metrics["backup_and_preservation_verified"] = True
        require(metrics["elapsed_seconds"] < 10, "repair exceeded unchanged 10-second CI budget")
        metrics.update(stage="complete", passed=True)


class SafetyTests(unittest.TestCase):
    def test_hash_mismatch_is_rejected_before_chmod(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "binary"
            path.write_bytes(b"wrong")
            path.chmod(0o600)
            with self.assertRaisesRegex(RuntimeError, "SHA-256"):
                verify_binary(path, ("0" * 64, 5))
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)

    def test_child_environment_has_no_inherited_credentials(self):
        env = child_environment(Path("/isolated-fixture"))
        self.assertEqual(set(env), {"PATH", "HOME", "USERPROFILE", "XDG_CONFIG_HOME",
                                  "LIBRA_CONFIG_GLOBAL_DB", "LIBRA_CONFIG_SYSTEM_DB", "LIBRA_TEST"})
        self.assertTrue(env["LIBRA_CONFIG_GLOBAL_DB"].startswith("/isolated-fixture/"))
        self.assertTrue(env["LIBRA_CONFIG_SYSTEM_DB"].startswith("/isolated-fixture/"))

    def test_timeout_terminates_owned_child(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaisesRegex(RuntimeError, "bounded execution time"):
                run_child([sys.executable, "-c", "import time; time.sleep(10)"], root,
                          child_environment(root), "synthetic timeout", timeout=0.05)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--metrics", type=Path)
    parser.add_argument("--binaries-dir", type=Path)
    parser.add_argument("--allow-local", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    os.umask(0o077)
    if args.self_test:
        result = unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(SafetyTests))
        return 0 if result.wasSuccessful() else 1
    if args.metrics is None:
        parser.error("--metrics is required")
    metrics = {"passed": False, "budget_seconds": 10, "release_source": RELEASE_SOURCE,
               "binary_pins": PINS, "stage": "initialization"}
    try:
        benchmark(args, metrics)
    except RuntimeError as error:
        # Only our static, synthetic-context messages; never forward engine/CLI output.
        metrics["failure_type"] = type(error).__name__
        print(f"MIG-06 failed at {metrics['stage']}: {error}", file=sys.stderr)
        return_code = 1
    except Exception as error:
        metrics["failure_type"] = type(error).__name__
        print(f"MIG-06 failed at {metrics['stage']}; gate remains unproven", file=sys.stderr)
        return_code = 1
    else:
        return_code = 0
    args.metrics.write_text(json.dumps(metrics, indent=2) + "\n")
    print(json.dumps(metrics), flush=True)
    return return_code


if __name__ == "__main__":
    sys.exit(main())
