#!/usr/bin/env python3
import os
import pathlib
import subprocess
import sys
import tarfile
import tempfile
import zipfile


def archive_names(path: pathlib.Path) -> list[str]:
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as archive:
            return archive.namelist()
    with tarfile.open(path, "r:xz") as archive:
        return archive.getnames()


def extract(path: pathlib.Path, destination: pathlib.Path) -> None:
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as archive:
            archive.extractall(destination)
    else:
        with tarfile.open(path, "r:xz") as archive:
            archive.extractall(destination)


def main() -> None:
    artifact_root = pathlib.Path(sys.argv[1])
    target = sys.argv[2]
    archives = [
        path
        for path in artifact_root.rglob(f"*{target}*")
        if path.name.endswith((".tar.xz", ".zip"))
    ]
    if len(archives) != 1:
        raise SystemExit(f"expected one archive for {target}, found {archives}")
    archive = archives[0]
    names = archive_names(archive)
    required = [
        "LICENSE",
        "NOTICE.md",
        "licenses/CLINIC-MIT.txt",
        "licenses/INFERNO-CDDL-1.0.txt",
    ]
    for required_path in required:
        if not any(name.endswith(required_path) for name in names):
            raise SystemExit(f"{archive} is missing {required_path}")
    executable_name = "v8scope.exe" if "windows" in target else "v8scope"
    if not any(name.endswith(executable_name) for name in names):
        raise SystemExit(f"{archive} is missing {executable_name}")

    with tempfile.TemporaryDirectory() as directory:
        destination = pathlib.Path(directory)
        extract(archive, destination)
        executables = [
            path
            for path in destination.rglob(executable_name)
            if path.is_file()
        ]
        if len(executables) != 1:
            raise SystemExit(f"expected one extracted executable, found {executables}")
        if os.name != "nt":
            executables[0].chmod(executables[0].stat().st_mode | 0o111)
        completed = subprocess.run(
            [str(executables[0]), "--version"],
            check=True,
            capture_output=True,
            text=True,
        )
        if "v8scope" not in completed.stdout:
            raise SystemExit(f"unexpected version output: {completed.stdout!r}")
    print(f"smoke tested {archive.name}")


if __name__ == "__main__":
    main()
