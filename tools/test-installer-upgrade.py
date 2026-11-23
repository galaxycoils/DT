"""Exercise the published installers on disposable GitHub-hosted runners."""
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
from contextlib import closing


def run(*args):
    subprocess.run(list(args), check=True)


def install(tag, scratch):
    package, = Path("downloads", tag).glob("*" + os.environ["SUFFIX"])
    package = package.resolve()
    checksum, name = Path(str(package) + ".sha256").read_text().split()
    digest = hashlib.sha256()
    with package.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    if name != package.name or digest.hexdigest() != checksum:
        raise ValueError(f"Checksum mismatch for {package.name}")
    if sys.platform == "darwin":
        mount = scratch / "mount"
        mount.mkdir(exist_ok=True)
        run("hdiutil", "attach", package, "-mountpoint", mount, "-readonly", "-nobrowse")
        try:
            run("sudo", "installer", "-pkg", mount / "Install DT.pkg", "-target", "/")
        finally:
            run("hdiutil", "detach", mount)
        bins = Path("/Applications/DT.app/Contents/MacOS")
    elif sys.platform == "win32":
        run(package, "/S")
        bins = Path(os.environ["ProgramFiles"], "DT", "bin")
    elif sys.platform.startswith("linux"):
        run("sudo", "apt-get", "install", "-y", str(package))
        bins = Path("/usr/bin")
    else:
        raise ValueError(f"Unsupported platform {sys.platform}")
    ext = ".exe" if sys.platform == "win32" else ""
    cli = bins / ("dt" + ext)
    version = subprocess.check_output([cli, "--version"], text=True).strip()
    if version != "dt " + tag[1:]:
        raise ValueError(f"Expected {tag}, installed CLI reported {version}")
    return cli, bins / ("dt-doctor" + ext)


def main():
    if os.environ.get("DT_INSTALLER_RUNNER") != "github-hosted":
        sys.exit("This test installs system packages on a disposable GitHub-hosted runner.")
    if sys.platform.startswith("linux"):
        run("sudo", "apt-get", "-o", "Acquire::Retries=3", "update")
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        store = root / "profile"
        folder = store / "memory.sqlite"
        folder.mkdir(parents=True)
        config = root / "config.json"
        config.write_text(json.dumps({"storage_dir": str(store)}))
        env = dict(os.environ, GROK_MUSE_DATA_DIR=str(store))
        cli, _ = install(os.environ["FROM_TAG"], root)
        db_path = folder / "memory.db"
        with closing(sqlite3.connect(db_path)) as db:
            db.executescript("CREATE TABLE sample(body TEXT); INSERT INTO sample VALUES ('saved');")
        (store / "dt-schema-version.json").write_text(json.dumps({"schema_version": 1}))
        subprocess.run([cli, "--config", config, "doctor", "check", "--json"], env=env,
                       stdout=subprocess.PIPE, check=True)
        cli, doctor = install(os.environ["TO_TAG"], root)
        with closing(sqlite3.connect(db_path)) as db:
            db.executescript("PRAGMA journal_mode=WAL; INSERT INTO sample VALUES ('wal');")
            subprocess.run([doctor, "--config", config, "repair", "--json"], env=env,
                           stdout=subprocess.PIPE, check=True)
        manifest, = (store / "snapshots").glob("*/rollback.json")
        with closing(sqlite3.connect(db_path)) as db:
            db.execute("DELETE FROM sample")
            db.commit()
        snapshot_id = json.loads(manifest.read_text())["snapshot_id"]
        subprocess.run([cli, "--config", config, "doctor", "restore", snapshot_id, "--json"],
                       env=env, stdout=subprocess.PIPE, check=True)
        with closing(sqlite3.connect(db_path)) as db:
            assert db.execute("SELECT body FROM sample ORDER BY rowid").fetchall() == [
                ("saved",), ("wal",)
            ]
        print(f"{os.environ['FROM_TAG']} to {os.environ['TO_TAG']} passed install, upgrade and restore")


if __name__ == "__main__":
    main()
