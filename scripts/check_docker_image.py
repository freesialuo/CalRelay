"""Isolated real-Engine packaging smoke; no production sources or credentials."""
import io
import json
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
import uuid

IMAGE = sys.argv[1] if len(sys.argv) == 2 else "calrelay:local"
PREFIX = "calrelay-packaging-" + uuid.uuid4().hex[:12]
CONTAINER = PREFIX + "-service"
SEED = PREFIX + "-seed"
VOLUMES = [PREFIX + suffix for suffix in ("-data", "-config", "-secrets")]
MARKER = b"CALRELAY_RUNTIME_SECRET_CANARY_105_45d821"


def docker(*args, check=True):
    return subprocess.run(["docker", *args], check=check, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=180).stdout


def inspect():
    return json.loads(docker("inspect", CONTAINER))[0]


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def get(base, path):
    request = urllib.request.Request(base + path)
    try:
        with urllib.request.urlopen(request, timeout=3) as response:
            return response.status, response.headers, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.headers, error.read()


def snapshot(directory, name):
    target = directory / name
    docker("cp", CONTAINER + ":/data/calrelay.db", str(target))
    with sqlite3.connect(target) as database:
        require(database.execute("PRAGMA integrity_check").fetchone()[0] == "ok", "SQLite integrity")
        require(MARKER not in target.read_bytes(), "Bark secret must not be stored as plaintext")
        return (
            database.execute("SELECT version, installed_on FROM _sqlx_migrations ORDER BY version").fetchall(),
            database.execute("SELECT id, kind, key_version, revision FROM credentials WHERE id='packaging-bark'").fetchone(),
        )


try:
    config = json.loads(docker("image", "inspect", IMAGE))[0]["Config"]
    require(config["User"] == "65532:65532", "image must run as nonroot")
    require(config["Entrypoint"] == ["/usr/local/bin/calrelay"], "exec entrypoint")
    require(config["StopSignal"] == "SIGTERM", "stop signal")
    probe = config["Healthcheck"]
    require(probe["Test"][1:4] == ["/usr/local/bin/calrelay", "healthcheck", "live"], "real CLI live probe")
    require(probe["StartPeriod"] == 60_000_000_000 and probe["Retries"] == 3, "probe startup/retries")
    for volume in VOLUMES:
        docker("volume", "create", volume)
    docker("create", "--name", SEED,
           "--mount", "type=volume,src=" + VOLUMES[1] + ",dst=/run/calrelay",
           "--mount", "type=volume,src=" + VOLUMES[2] + ",dst=/run/secrets", IMAGE)
    with tempfile.TemporaryDirectory(prefix=PREFIX) as temporary:
        directory = Path(temporary)
        deployment = directory / "deployment.json"
        configuration = json.loads((Path(__file__).resolve().parents[1] / "examples/docker-deployment.json").read_text())
        deployment.write_text(json.dumps(configuration))
        turnstile = directory / "turnstile"
        turnstile.write_bytes(MARKER + b"_TURNSTILE")
        configuration["admin"]["turnstile_secret_file"] = "/run/secrets/intentionally-missing"
        configuration["credentials"]["key_files"] = {"1": "/run/secrets/intentionally-missing"}
        probe_only = directory / "probe-only.json"
        probe_only.write_text(json.dumps(configuration))
        master = directory / "master"
        master.write_bytes(bytes(range(32)))  # Synthetic CI material, never copied into the image.
        secret = directory / "canary"
        secret.write_bytes(MARKER)
        docker("cp", str(deployment), SEED + ":/run/calrelay/deployment.json")
        docker("cp", str(secret), SEED + ":/run/secrets/canary")
        docker("cp", str(master), SEED + ":/run/secrets/calrelay-master-v1")
        docker("cp", str(turnstile), SEED + ":/run/secrets/turnstile")
        docker("cp", str(probe_only), SEED + ":/run/calrelay/probe-only.json")
        docker("run", "--detach", "--name", CONTAINER, "--read-only",
               "--user", "65532:65532", "--cap-drop", "ALL",
               "--security-opt", "no-new-privileges", "--pids-limit", "128",
               "--memory", "512m", "--cpus", "1", "--restart", "unless-stopped",
               "--stop-timeout", "30", "--tmpfs", "/tmp:rw,size=16777216,mode=1777",
               "--publish", "127.0.0.1::3000",
               "--mount", "type=volume,src=" + VOLUMES[0] + ",dst=/data",
               "--mount", "type=volume,src=" + VOLUMES[1] + ",dst=/run/calrelay,readonly",
               "--mount", "type=volume,src=" + VOLUMES[2] + ",dst=/run/secrets,readonly", IMAGE)
        state = inspect()
        require(state["HostConfig"]["ReadonlyRootfs"], "read-only root")
        mounts = {entry["Destination"]: entry for entry in state["Mounts"]}
        require(not mounts["/run/calrelay"]["RW"] and not mounts["/run/secrets"]["RW"], "read-only config/secrets")
        port = state["NetworkSettings"]["Ports"]["3000/tcp"][0]["HostPort"]
        base = "http://127.0.0.1:" + port
        deadline = time.monotonic() + 45
        while True:
            try:
                if get(base, "/health/live")[0] == 200:
                    break
            except (OSError, urllib.error.URLError):
                pass
            require(time.monotonic() < deadline, "real HTTP startup deadline")
            time.sleep(0.1)
        status, headers, body = get(base, "/admin/")
        require(status == 200 and b"CalRelay" in body, "built homepage")
        require(headers["Cache-Control"] == "no-store" and headers["X-Content-Type-Options"] == "nosniff", "homepage headers")
        assets = re.findall(rb'"(/admin/assets/[^" ]+)"', body)
        require(len(assets) == 2, "built JS/CSS references")
        for asset in assets:
            status, headers, body = get(base, asset.decode())
            require(status == 200 and body and "immutable" in headers["Cache-Control"], "real built asset")
        require(json.loads(get(base, "/api/v1/version")[2])["name"] == "CalRelay", "real API coexistence")
        require(get(base, "/api/admin/v1/sources")[0] in (401, 403), "real management auth must reject missing identity")
        for path in ("/api/unknown", "/admin/assets/%2e%2e%2fdeployment.json", "/run/secrets/canary"):
            require(get(base, path)[0] == 404, "path boundary " + path)
        readiness = get(base, "/health/ready")[0]
        require(readiness in (200, 503), "readiness must report the real runtime state")
        print("Observed runtime readiness HTTP", readiness, flush=True)
        docker("exec", CONTAINER, "/usr/local/bin/calrelay", "healthcheck", "live",
               "--deployment-config", "/run/calrelay/deployment.json", "--timeout-ms", "2000")
        docker("exec", "-e", "CALRELAY_DATABASE=/run/secrets/no-database", CONTAINER,
               "/usr/local/bin/calrelay", "healthcheck", "live", "--deployment-config",
               "/run/calrelay/probe-only.json", "--timeout-ms", "2000")
        while inspect()["State"]["Health"]["Status"] != "healthy":
            require(time.monotonic() < deadline, "Docker live health deadline")
            time.sleep(0.2)
        require(inspect()["RestartCount"] == 0, "ready=false must not cause restart")
        receipt = json.loads(docker("exec", CONTAINER, "/usr/local/bin/calrelay", "credential", "provision-bark",
                                    "--deployment-config", "/run/calrelay/deployment.json",
                                    "--credential-id", "packaging-bark", "--secret-file", "/run/secrets/canary"))
        require(receipt == {"credential_id": "packaging-bark", "revision": 0}, "real create-only credential CLI")
        denied = subprocess.run(["docker", "exec", "-e", "CALRELAY_DATABASE=/run/calrelay/blocked.db",
                                 CONTAINER, "/usr/local/bin/calrelay", "--deployment-config",
                                 "/run/calrelay/deployment.json"], capture_output=True, timeout=10)
        require(denied.returncode != 0, "read-only configuration must reject writes")
        start = time.monotonic()
        docker("stop", "--time", "30", CONTAINER)
        state = inspect()
        require(state["State"]["ExitCode"] == 0 and not state["State"]["OOMKilled"], "SIGTERM clean exit")
        require(time.monotonic() - start < 30, "stop budget")
        before = snapshot(directory, "before.db")
        require(before[0] and before[0][-1][0] >= 13, "real migrations persisted")
        require(before[1] == ("packaging-bark", "bark", 1, 0), "encrypted CLI credential persisted")
        docker("start", CONTAINER)
        port = inspect()["NetworkSettings"]["Ports"]["3000/tcp"][0]["HostPort"]
        base = "http://127.0.0.1:" + port
        deadline = time.monotonic() + 20
        while True:
            try:
                if get(base, "/health/live")[0] == 200:
                    break
            except (OSError, urllib.error.URLError):
                pass
            require(time.monotonic() < deadline, "persistent restart deadline")
            time.sleep(0.1)
        rotated = json.loads(docker("exec", CONTAINER, "/usr/local/bin/calrelay", "credential", "rotate-bark",
                                    "--deployment-config", "/run/calrelay/deployment.json",
                                    "--credential-id", "packaging-bark", "--expected-revision", "0"))
        require(rotated == {"credential_id": "packaging-bark", "revision": 1}, "real decrypt/rotate after reopen")
        docker("stop", "--time", "30", CONTAINER)
        after = snapshot(directory, "after.db")
        require(after[0] == before[0] and after[1] == ("packaging-bark", "bark", 1, 1), "migration and credential history survive reopen")
        archive = docker("export", CONTAINER)
        require(MARKER not in archive, "runtime secret must not enter exported root filesystem")
        with tarfile.open(fileobj=io.BytesIO(archive)) as filesystem:
            names = set(filesystem.getnames())
        require("usr/local/bin/calrelay" in names, "standard binary present")
        require("etc/ssl/certs/ca-certificates.crt" in names and "usr/share/zoneinfo/UTC" in names, "CA and timezone data")
        require(not any("harness" in name or "node_modules" in name or name.endswith("/node") or name.startswith("build/") for name in names), "no harness/Node/source tree")
    print("PASS: real image HTTP/assets/API/live, honest readiness, nonroot/read-only mounts, SIGTERM, SQLite reopen, runtime contents")
    print("UNVERIFIED: production workers/credentials/UI journey, arm64, external recovery/Engine stop-kill races, upgrades/scanning/SBOM")
finally:
    for container in (CONTAINER, SEED):
        docker("rm", "--force", "--volumes", container, check=False)
    for volume in VOLUMES:
        docker("volume", "rm", volume, check=False)
