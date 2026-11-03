# Executed only inside the approved development container. stdout is a deterministic tar.
import io, os, pathlib, stat, sys, tarfile
root = pathlib.Path('/home/coder/project')
relative = pathlib.PurePosixPath(sys.argv[1])
assert not relative.is_absolute() and '..' not in relative.parts
base = root.joinpath(relative)
assert base.resolve().is_relative_to(root.resolve()) and base.is_dir()
for parent in [base, *base.parents]:
    if parent == root.parent: break
    assert not parent.is_symlink(), 'symlink project path rejected'
files = []; total = 0
excluded = {'.git', '.ssh', '.gnupg', 'node_modules', '.venv', 'venv', '__pycache__', 'target', 'dist'}
for directory, dirs, names in os.walk(base, followlinks=False):
    dirs[:] = sorted(d for d in dirs if d not in excluded and not d.startswith('.env'))
    for d in dirs: assert not pathlib.Path(directory, d).is_symlink(), 'symlink directory rejected'
    for name in sorted(names):
        if name in excluded or name.startswith('.env') or name.endswith(('.pem', '.key')): continue
        path = pathlib.Path(directory, name); before = path.lstat()
        assert stat.S_ISREG(before.st_mode) and before.st_nlink == 1, 'non-regular or hardlinked source rejected'
        assert before.st_size <= 8*1024*1024, 'source file too large'
        with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW), 'rb') as handle:
            opened = os.fstat(handle.fileno())
            assert (opened.st_dev, opened.st_ino) == (before.st_dev, before.st_ino)
            data = handle.read(8*1024*1024+1)
            after = os.fstat(handle.fileno())
        assert len(data) == before.st_size and (before.st_mtime_ns, before.st_size) == (after.st_mtime_ns, after.st_size), 'source changed during capture'
        total += len(data); files.append((path.relative_to(base).as_posix(), data, 0o755 if before.st_mode & 0o111 else 0o644))
        assert total <= 24*1024*1024 and len(files) <= 2048, 'snapshot limit exceeded'
assert {'Dockerfile', '.cofleet-release.json'}.issubset({name for name, _, _ in files}), 'Dockerfile and .cofleet-release.json required'
with tarfile.open(fileobj=sys.stdout.buffer, mode='w|', format=tarfile.USTAR_FORMAT) as archive:
    for name, data, mode in sorted(files):
        info = tarfile.TarInfo(name); info.size = len(data); info.mode = mode; info.mtime = 0
        archive.addfile(info, io.BytesIO(data))
