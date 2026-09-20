# Index v3 fixtures (plan issues/490 SW-01)

`git-2.55-skip-worktree.index` is a real Git index version 3 fixture, 104 bytes,
generated with **Git 2.55.0** and checked in so the automated tests never call
the system Git:

```sh
cd "$(mktemp -d)" && git init -q .
echo hi > a.txt
git add a.txt
git update-index --skip-worktree a.txt
git update-index --index-version 3
cp .git/index git-2.55-skip-worktree.index
```

Layout (SHA-1, one entry, `a.txt`):

```
00  DIRC
04  version = 3
08  entry count = 1
0c  entry: ctime(8) mtime(8) dev(4) ino(4) mode(4) uid(4) gid(4) size(4)
34  object id (20 bytes, blob "hi\n" = 45b983be36b73c0788dc9cbcb76cbb80fc7bb057)
48  main flags = 0x4005 (CE_EXTENDED | name length 5)
4a  extended flags = 0x4000 (CE_SKIP_WORKTREE)
4c  name "a.txt" + NUL padding to the 8-byte entry alignment
54  SHA-1 trailer
```

The extended flags word sits **between the main flags and the name**; the
entry's padding is computed on `hash_len + 2 + 2 + name_len`. Unknown extended
bits (anything outside `0x4000`/`0x2000`) must fail closed.
