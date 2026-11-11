#!/usr/bin/env python3
"""Independent check that every photo and video on a card has an identical copy on a backup drive.

    python3 scripts/verify-backup.py /Volumes/CARD /Volumes/SSD

Shares no code with Darkroom: it reads both sides itself and compares SHA-256 of whole files, wherever
the copies are and whatever they are called. It only reads (files are opened read-only; nothing is
created, changed or deleted on either side). Exit code 0 = everything on the card is on the drive,
1 = something is missing, 2 = it couldn't read something (so it can't say).

Run it before formatting a card. It reads the whole card again, so it takes about as long as a copy.
"""
import hashlib
import os
import sys

# What Darkroom backs up (crates/cull-core/src/offload/kind.rs). Other files are listed, not checked.
MEDIA = {
    "jpg", "jpeg",
    "nef", "nrw", "raf", "dng", "cr2", "cr3", "arw", "orf", "rw2", "pef",
    "hif", "heic", "heif",
    "mov", "mp4", "m4v", "avi", "mts", "m2ts", "nev",
}


def files(root):
    """Every regular file under root: (path, size). Hidden names are skipped, links are not followed."""
    errors = []
    out = []
    for folder, dirs, names in os.walk(root, onerror=errors.append, followlinks=False):
        dirs[:] = [d for d in dirs if not d.startswith(".")]
        for name in names:
            if name.startswith("."):
                continue
            path = os.path.join(folder, name)
            try:
                st = os.lstat(path)
            except OSError as e:
                errors.append(e)
                continue
            if os.path.isfile(path) and not os.path.islink(path):
                out.append((path, st.st_size))
    return out, errors


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:  # read-only
        while True:
            chunk = f.read(4 << 20)
            if not chunk:
                return h.hexdigest()
            h.update(chunk)


def is_media(path):
    return os.path.splitext(path)[1].lstrip(".").lower() in MEDIA


def main():
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    card, drive = (os.path.realpath(p) for p in sys.argv[1:])
    for p in (card, drive):
        if not os.path.isdir(p):
            print(f"Not a folder: {p}")
            return 2
    if card == drive or drive.startswith(card + os.sep) or card.startswith(drive + os.sep):
        print("The card and the drive must be two separate folders.")
        return 2

    card_files, card_errors = files(card)
    drive_files, drive_errors = files(drive)
    media = [(p, s) for p, s in card_files if is_media(p)]
    other = [p for p, _ in card_files if not is_media(p)]
    print(f"Card:  {card}\nDrive: {drive}")
    print(f"{len(media)} photos and videos on the card; {len(drive_files)} files on the drive.")

    # Only drive files as big as some card file can be a copy of it.
    by_size = {}
    for p, s in drive_files:
        by_size.setdefault(s, []).append(p)
    drive_hashes = {}  # size -> set of hashes, filled when first needed
    unreadable = [str(e) for e in card_errors]
    drive_unreadable = [str(e) for e in drive_errors]
    missing = []
    found = 0
    for i, (path, size) in enumerate(sorted(media), 1):
        print(f"\r  checking {i}/{len(media)}", end="", flush=True)
        try:
            want = sha256(path)
        except OSError as e:
            unreadable.append(f"{path}: {e}")
            continue
        if size not in drive_hashes:
            hashes = set()
            for candidate in by_size.get(size, []):
                try:
                    hashes.add(sha256(candidate))
                except OSError as e:
                    drive_unreadable.append(f"{candidate}: {e}")
            drive_hashes[size] = hashes
        if want in drive_hashes[size]:
            found += 1
        else:
            missing.append(os.path.relpath(path, card))
    print()

    print(f"{found} of {len(media)} have an identical copy on the drive.")
    if other:
        kinds = {}
        for p in other:
            ext = os.path.splitext(p)[1].lstrip(".").upper() or "(no extension)"
            kinds[ext] = kinds.get(ext, 0) + 1
        print("Not checked (not photos or videos): " + ", ".join(f"{n} {k}" for k, n in sorted(kinds.items())))
    if drive_unreadable:
        print(f"\n{len(drive_unreadable)} places on the drive couldn't be read (copies there don't count):")
        for line in drive_unreadable[:20]:
            print(f"  {line}")
    if missing:
        print(f"\nNOT ON THE DRIVE ({len(missing)}):")
        for m in missing:
            print(f"  {m}")
    if unreadable:
        print(f"\nCOULDN'T READ ON THE CARD ({len(unreadable)}), so these are not confirmed:")
        for line in unreadable:
            print(f"  {line}")
    if missing:
        print("\nDo NOT format this card.")
        return 1
    if unreadable:
        print("\nDo NOT format this card until these can be read and checked.")
        return 2
    print("\nEvery photo and video on the card has an identical copy on the drive.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
